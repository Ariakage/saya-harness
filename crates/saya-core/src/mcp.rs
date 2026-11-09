//! Bounded MCP client connections. Authorization and process creation remain host duties.
//!
//! A tool call is sent once. A lost connection, cancellation, or server request for
//! additional input never causes this adapter to replay an operation.

mod adapter;
pub mod config;
mod http;
mod projection;
mod wire;
pub use adapter::McpToolAdapter;
pub use http::McpHttpClient;

use crate::boundary::Cancellation;
use crate::contract::{BoundaryError, ErrorInfo};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ClientCapabilities, ClientConfig, Implementation,
    PaginatedRequestParams, ProtocolVersion, ServerNotification, SubscriptionFilterBuilder,
};
use rmcp::service::{ClientInitializeError, NotificationContext, RunningService};
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt, RoleClient, ServiceError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

/// Limits apply to discovery as a whole, individual wire messages, and projected results.
#[derive(Debug, Clone)]
pub struct McpLimits {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub max_wire_bytes: usize,
    pub max_pages: usize,
    pub max_tools: usize,
    pub max_catalog_bytes: usize,
    pub max_result_bytes: usize,
    pub max_content_blocks: usize,
}

impl Default for McpLimits {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(20),
            request_timeout: Duration::from_secs(120),
            max_wire_bytes: 1024 * 1024,
            max_pages: 32,
            max_tools: 256,
            max_catalog_bytes: 1024 * 1024,
            max_result_bytes: 256 * 1024,
            max_content_blocks: 128,
        }
    }
}

impl McpLimits {
    fn validate(&self) -> Result<(), BoundaryError> {
        if self.connect_timeout.is_zero()
            || self.request_timeout.is_zero()
            || self.max_wire_bytes == 0
            || self.max_pages == 0
            || self.max_tools == 0
            || self.max_catalog_bytes == 0
            || self.max_result_bytes < 256
            || self.max_content_blocks == 0
        {
            return Err(error("invalid_config", "MCP limits must be positive"));
        }
        Ok(())
    }
}

/// Transient HTTP settings. Tokens and headers are never included in Debug output.
pub struct McpHttpConfig {
    pub endpoint: String,
    pub bearer_token: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub allow_insecure_http: bool,
}

impl fmt::Debug for McpHttpConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpHttpConfig")
            .field("authentication", &self.bearer_token.is_some())
            .field("header_count", &self.headers.len())
            .field("allow_insecure_http", &self.allow_insecure_http)
            .finish()
    }
}

impl McpHttpConfig {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            bearer_token: None,
            headers: BTreeMap::new(),
            allow_insecure_http: false,
        }
    }
}

/// Server-provided descriptions and annotations are data, never permission grants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    pub annotations: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpCatalog {
    pub generation: u64,
    pub tools: Vec<McpTool>,
}

/// Safe, text-first projection. Binary payloads are described without passing base64 to models.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpContent {
    Text {
        text: String,
    },
    Resource {
        uri: String,
        mime_type: Option<String>,
        text: String,
    },
    ResourceLink {
        uri: String,
        name: String,
        mime_type: Option<String>,
    },
    Omitted {
        kind: String,
        mime_type: Option<String>,
        encoded_bytes: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolResult {
    pub content: Vec<McpContent>,
    pub structured_content: Option<Value>,
    pub is_error: bool,
    pub truncated: bool,
}

struct Handler {
    generation: Arc<AtomicU64>,
}

struct ValidatedCatalog {
    snapshot: McpCatalog,
    schemas: BTreeMap<
        String,
        (
            Arc<jsonschema::Validator>,
            Option<Arc<jsonschema::Validator>>,
        ),
    >,
}

impl ClientHandler for Handler {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("saya-harness", env!("CARGO_PKG_VERSION")),
        )
    }

    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }
}

/// One controlled connection, with no persistent credentials and no subprocess ownership.
pub struct McpClient {
    server_id: String,
    connection_id: uuid::Uuid,
    service: RunningService<RoleClient, Handler>,
    generation: Arc<AtomicU64>,
    catalog: RwLock<Option<ValidatedCatalog>>,
    notifications_alive: Arc<AtomicBool>,
    limits: McpLimits,
    notifications: Option<NotificationTask>,
}

struct NotificationTask(JoinHandle<()>);

impl Drop for NotificationTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl fmt::Debug for McpClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpClient")
            .field("server_id", &self.server_id)
            .field("generation", &self.generation())
            .finish()
    }
}

impl McpClient {
    /// Connect to pipes supplied by a host that has authorized and isolated the server.
    /// This method never spawns a process or inherits environment variables.
    pub async fn connect_stdio<R, W>(
        server_id: impl Into<String>,
        reader: R,
        writer: W,
        limits: McpLimits,
        cancel: &dyn Cancellation,
    ) -> Result<Self, BoundaryError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let writer = wire::WireWriter::new(writer, limits.max_wire_bytes);
        let transport = AsyncRwTransport::<RoleClient, _, _>::new(reader, writer)
            .with_max_line_length(limits.max_wire_bytes);
        Self::connect(server_id.into(), transport, limits, cancel).await
    }

    /// Connect to a configured endpoint with an isolated HTTP client and disabled redirects.
    /// HTTP is accepted only through an explicit transport opt-in.
    pub async fn connect_http(
        server_id: impl Into<String>,
        config: McpHttpConfig,
        limits: McpLimits,
        cancel: &dyn Cancellation,
    ) -> Result<Self, BoundaryError> {
        let client = McpHttpClient::new(&limits)?;
        Self::connect_http_with_client(server_id, client, config, limits, cancel).await
    }

    /// Accept a host transport preserving single-send, TLS, redirect, and early byte limits.
    /// Use `McpHttpClient::with_auth_manager` for OAuth without tool-call replay.
    pub async fn connect_http_with_client<C>(
        server_id: impl Into<String>,
        client: C,
        config: McpHttpConfig,
        limits: McpLimits,
        cancel: &dyn Cancellation,
    ) -> Result<Self, BoundaryError>
    where
        C: StreamableHttpClient,
    {
        let transport_config = http_transport_config(config, &limits)?;
        let transport = StreamableHttpClientTransport::with_client(client, transport_config);
        Self::connect(server_id.into(), transport, limits, cancel).await
    }

    async fn connect<T, E, A>(
        server_id: String,
        transport: T,
        limits: McpLimits,
        cancel: &dyn Cancellation,
    ) -> Result<Self, BoundaryError>
    where
        T: rmcp::transport::IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        limits.validate()?;
        if !crate::registry::valid_server_id(&server_id) {
            return Err(error("invalid_config", "MCP server identifier is invalid"));
        }
        let generation = Arc::new(AtomicU64::new(1));
        let handler = Handler {
            generation: generation.clone(),
        };
        let service = bounded(
            handler.serve_with_lifecycle(
                transport,
                ClientLifecycleMode::Auto {
                    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                    legacy_version: Some(ProtocolVersion::V_2025_11_25),
                },
            ),
            limits.connect_timeout,
            cancel,
        )
        .await?
        .map_err(connect_error)?;
        let mut client = Self {
            server_id,
            connection_id: uuid::Uuid::now_v7(),
            service,
            generation,
            catalog: RwLock::new(None),
            notifications_alive: Arc::new(AtomicBool::new(true)),
            limits,
            notifications: None,
        };
        client.start_notifications(cancel).await?;
        Ok(client)
    }

    async fn start_notifications(
        &mut self,
        cancel: &dyn Cancellation,
    ) -> Result<(), BoundaryError> {
        let Some(info) = self.service.peer_info() else {
            return Err(error("mcp_protocol", "MCP peer metadata is missing"));
        };
        let list_changed = info
            .capabilities
            .tools
            .as_ref()
            .is_some_and(|tools| tools.list_changed == Some(true));
        if info.protocol_version != ProtocolVersion::V_2026_07_28 || !list_changed {
            return Ok(());
        }
        let mut subscription = bounded(
            self.service.listen(
                SubscriptionFilterBuilder::default()
                    .tools_list_changed()
                    .build(),
            ),
            self.limits.connect_timeout,
            cancel,
        )
        .await?
        .map_err(service_error)?;
        let generation = self.generation.clone();
        let alive = self.notifications_alive.clone();
        self.notifications = Some(NotificationTask(tokio::spawn(async move {
            while let Ok(Some(notification)) = subscription.next().await {
                if matches!(
                    notification,
                    ServerNotification::ToolListChangedNotification(_)
                ) {
                    generation.fetch_add(1, Ordering::AcqRel);
                }
            }
            // A lost change stream makes every previously approved catalogue stale.
            alive.store(false, Ordering::Release);
            generation.fetch_add(1, Ordering::AcqRel);
        })));
        Ok(())
    }

    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    /// Opaque registry revision unique across reconnects as well as catalogue changes.
    pub fn revision(&self) -> String {
        format!("{}:{}", self.connection_id, self.generation())
    }

    /// Changes when a server changes its catalogue or the change stream is lost.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// A lost notification stream requires a new connection before approvals can be issued.
    pub fn is_connected(&self) -> bool {
        !self.service.is_closed() && self.notifications_alive.load(Ordering::Acquire)
    }

    /// Invalidate and cancel a shared connection immediately, for host revocation/disconnect.
    /// The host must still reap its process and may use `shutdown` to await protocol cleanup.
    pub fn cancel_connection(&self) {
        self.notifications_alive.store(false, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.service.cancellation_token().cancel();
    }

    fn ensure_connected(&self) -> Result<(), BoundaryError> {
        if !self.is_connected() {
            return Err(error(
                "mcp_transport",
                "MCP connection or change stream has closed",
            ));
        }
        Ok(())
    }

    /// Read a bounded, duplicate-free catalogue. Server pagination cannot bypass the total limit.
    pub async fn list_tools(&self, cancel: &dyn Cancellation) -> Result<McpCatalog, BoundaryError> {
        bounded(
            self.list_tools_inner(cancel),
            self.limits.request_timeout,
            cancel,
        )
        .await?
    }

    async fn list_tools_inner(
        &self,
        cancel: &dyn Cancellation,
    ) -> Result<McpCatalog, BoundaryError> {
        self.ensure_connected()?;
        let generation = self.generation();
        let mut tools = Vec::new();
        let mut schemas = BTreeMap::new();
        let mut names = BTreeSet::new();
        let mut cursors = BTreeSet::new();
        let mut cursor = None;
        let mut total_bytes = 0usize;
        for _ in 0..self.limits.max_pages {
            if cancel.is_cancelled() {
                return Err(error("cancelled", "MCP request cancelled"));
            }
            let mut params = PaginatedRequestParams::default();
            params.cursor = cursor;
            let page = self
                .service
                .list_tools(Some(params))
                .await
                .map_err(service_error)?;
            for tool in page.tools {
                let size = serde_json::to_vec(&tool)
                    .map_err(|_| error("mcp_protocol", "MCP schema encoding failed"))?
                    .len();
                total_bytes = total_bytes.saturating_add(size);
                if tools.len() >= self.limits.max_tools
                    || total_bytes > self.limits.max_catalog_bytes
                {
                    return Err(error(
                        "mcp_limit",
                        "MCP catalogue exceeds configured limits",
                    ));
                }
                if tool.name.is_empty()
                    || tool.name.len() > 128
                    || !tool
                        .name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
                    || !names.insert(tool.name.to_string())
                {
                    return Err(error(
                        "mcp_protocol",
                        "MCP catalogue has invalid or duplicate tool names",
                    ));
                }
                let input_schema = Value::Object((*tool.input_schema).clone());
                if input_schema.get("type").and_then(Value::as_str) != Some("object") {
                    return Err(error(
                        "mcp_schema",
                        "MCP input schema must declare an object",
                    ));
                }
                let input_validator = crate::registry::compile_external_schema(&input_schema)
                    .map_err(|_| {
                        error("mcp_schema", "MCP input schema is invalid or unsupported")
                    })?;
                let output_schema = tool
                    .output_schema
                    .as_ref()
                    .map(|schema| Value::Object((**schema).clone()));
                let output_validator = output_schema
                    .as_ref()
                    .map(|schema| {
                        crate::registry::compile_external_schema(schema).map_err(|_| {
                            error("mcp_schema", "MCP output schema is invalid or unsupported")
                        })
                    })
                    .transpose()?;
                schemas.insert(tool.name.to_string(), (input_validator, output_validator));
                tools.push(McpTool {
                    name: tool.name.to_string(),
                    description: tool
                        .description
                        .map(|text| text.to_string())
                        .unwrap_or_default(),
                    input_schema,
                    output_schema,
                    annotations: tool
                        .annotations
                        .and_then(|annotations| serde_json::to_value(annotations).ok()),
                });
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                if generation != self.generation() {
                    return Err(error(
                        "mcp_stale_catalog",
                        "MCP catalogue changed during discovery",
                    ));
                }
                tools.sort_by(|left, right| left.name.cmp(&right.name));
                let mut previous = self.catalog.write().await;
                let generation = if previous
                    .as_ref()
                    .is_some_and(|catalog| catalog.snapshot.tools != tools)
                {
                    let next = generation.checked_add(1).ok_or_else(|| {
                        error("mcp_limit", "MCP catalogue generation limit reached")
                    })?;
                    self.generation
                        .compare_exchange(generation, next, Ordering::AcqRel, Ordering::Acquire)
                        .map_err(|_| {
                            error(
                                "mcp_stale_catalog",
                                "MCP catalogue changed while discovery waited for publication",
                            )
                        })?;
                    next
                } else {
                    if generation != self.generation() {
                        return Err(error(
                            "mcp_stale_catalog",
                            "MCP catalogue changed while discovery waited for publication",
                        ));
                    }
                    generation
                };
                let catalog = McpCatalog { generation, tools };
                *previous = Some(ValidatedCatalog {
                    snapshot: catalog.clone(),
                    schemas,
                });
                return Ok(catalog);
            }
            if !cursors.insert(cursor.clone()) {
                return Err(error("mcp_protocol", "MCP pagination cursor repeated"));
            }
        }
        Err(error(
            "mcp_limit",
            "MCP catalogue pagination exceeds configured limit",
        ))
    }

    /// Validate against the approved catalogue generation, then send exactly one request.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        expected_generation: u64,
        cancel: &dyn Cancellation,
    ) -> Result<McpToolResult, BoundaryError> {
        self.call_tool_captured(name, arguments, expected_generation, cancel, None)
            .await
            .map(|value| value.0)
    }

    pub(crate) async fn call_tool_captured(
        &self,
        name: &str,
        arguments: Value,
        expected_generation: u64,
        cancel: &dyn Cancellation,
        capture: Option<(
            &crate::output_store::OutputStore,
            crate::output_store::OutputScope,
        )>,
    ) -> Result<
        (
            McpToolResult,
            Vec<crate::output_store::SealedOutput>,
            Vec<crate::contract::ErrorInfo>,
        ),
        BoundaryError,
    > {
        self.ensure_connected()?;
        let catalog = self.catalog.read().await;
        let catalog = catalog.as_ref().ok_or_else(|| {
            error(
                "mcp_stale_catalog",
                "MCP catalogue must be discovered before tool execution",
            )
        })?;
        if expected_generation != self.generation()
            || catalog.snapshot.generation != expected_generation
        {
            return Err(error(
                "mcp_stale_catalog",
                "MCP tool approval references a stale catalogue",
            ));
        }
        let (validator, output_validator) = catalog.schemas.get(name).ok_or_else(|| {
            error(
                "mcp_unknown_tool",
                "MCP tool is absent from the approved catalogue",
            )
        })?;
        if !validator.is_valid(&arguments) {
            return Err(error(
                "invalid_args",
                "MCP tool arguments do not match the approved schema",
            ));
        }
        let arguments = arguments
            .as_object()
            .cloned()
            .ok_or_else(|| error("invalid_args", "MCP tool arguments must be an object"))?;
        let output_validator = output_validator.clone();
        let request = CallToolRequestParams::new(name.to_owned()).with_arguments(arguments);
        if serde_json::to_vec(&request)
            .map_err(|_| error("invalid_args", "MCP arguments cannot be encoded"))?
            .len()
            > self.limits.max_wire_bytes
        {
            return Err(error(
                "mcp_limit",
                "MCP tool arguments exceed the wire-message limit",
            ));
        }
        if expected_generation != self.generation() {
            return Err(error(
                "mcp_stale_catalog",
                "MCP catalogue changed before tool dispatch",
            ));
        }
        let attempted = AtomicBool::new(false);
        let request = async {
            attempted.store(true, Ordering::Release);
            self.service.call_tool_once(request).await
        };
        let response = bounded(request, self.limits.request_timeout, cancel)
            .await
            .map_err(|failure| tool_failure(failure, attempted.load(Ordering::Acquire)))?
            .map_err(|failure| tool_failure(service_error(failure), true))?;
        match response {
            CallToolResponse::Complete(result) => {
                let accepted = (|| {
                    if !result.is_error.unwrap_or(false)
                        && let Some(validator) = output_validator
                    {
                        let structured = result.structured_content.as_ref().ok_or_else(|| {
                            error(
                                "mcp_schema",
                                "MCP tool omitted its declared structured output",
                            )
                        })?;
                        if !validator.is_valid(structured) {
                            return Err(error(
                                "mcp_schema",
                                "MCP structured output does not match its schema",
                            ));
                        }
                    }
                    let (outputs, capture_errors) = match capture {
                        Some((store, scope)) => projection::capture_text_result(
                            &result,
                            store,
                            scope,
                            self.limits.max_wire_bytes,
                        ),
                        None => (Vec::new(), Vec::new()),
                    };
                    projection::project_result(result, &self.limits)
                        .map(|result| (result, outputs, capture_errors))
                })();
                accepted.map_err(|failure|error("outcome_unknown",match failure.0.code.as_str(){
                    "mcp_schema"=>"MCP tool returned an invalid output schema. Its result cannot be accepted and the call was not replayed",
                    "mcp_limit"=>"MCP tool returned output above its limit. Its result cannot be accepted and the call was not replayed",
                    _=>"MCP tool output cannot be accepted. The call was not replayed",
                }))
            }
            CallToolResponse::InputRequired(_) => Err(error(
                "needs_input",
                "MCP tool requires additional user input",
            )),
            CallToolResponse::Task(_) => Err(error(
                "outcome_unknown",
                "MCP server started an unadvertised task. Its completion is unknown",
            )),
            _ => Err(error(
                "outcome_unknown",
                "MCP server returned an unsupported result. Tool completion is unknown",
            )),
        }
    }

    /// Close protocol transport. The host separately reaps or terminates its server process.
    pub async fn shutdown(mut self) -> Result<(), BoundaryError> {
        self.notifications.take();
        tokio::time::timeout(Duration::from_secs(5), self.service.cancel())
            .await
            .map_err(|_| error("mcp_timeout", "MCP shutdown timed out"))?
            .map_err(|_| error("mcp_transport", "MCP connection shutdown failed"))?;
        Ok(())
    }
}

fn http_transport_config(
    config: McpHttpConfig,
    limits: &McpLimits,
) -> Result<StreamableHttpClientTransportConfig, BoundaryError> {
    let url = reqwest_mcp::Url::parse(&config.endpoint)
        .map_err(|_| error("invalid_config", "MCP endpoint URL is invalid"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
        || !(url.scheme() == "https" || (url.scheme() == "http" && config.allow_insecure_http))
    {
        return Err(error(
            "invalid_config",
            "MCP endpoint requires HTTPS and no embedded credentials, query, or fragment",
        ));
    }
    let mut headers = reqwest_mcp::header::HeaderMap::new();
    for (name, value) in config.headers {
        let name = reqwest_mcp::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| error("invalid_config", "MCP header name is invalid"))?;
        if matches!(
            name.as_str(),
            "authorization"
                | "proxy-authorization"
                | "host"
                | "content-length"
                | "content-type"
                | "accept"
                | "connection"
                | "transfer-encoding"
        ) || name.as_str().starts_with("mcp-")
        {
            return Err(error(
                "invalid_config",
                "MCP custom header conflicts with a transport-owned header",
            ));
        }
        let mut value = reqwest_mcp::header::HeaderValue::from_str(&value)
            .map_err(|_| error("invalid_config", "MCP header value is invalid"))?;
        value.set_sensitive(true);
        headers.insert(name, value);
    }
    let mut transport = StreamableHttpClientTransportConfig::with_uri(config.endpoint)
        .max_concurrent_requests(1)
        .max_sse_event_size(limits.max_wire_bytes)
        .reinit_on_expired_session(false)
        .custom_headers(
            headers
                .into_iter()
                .filter_map(|(name, value)| name.map(|name| (name, value)))
                .collect(),
        );
    if let Some(token) = config.bearer_token {
        if reqwest_mcp::header::HeaderValue::from_str(&format!("Bearer {token}")).is_err() {
            return Err(error(
                "invalid_config",
                "MCP bearer credential contains invalid header bytes",
            ));
        }
        transport = transport.auth_header(token);
    }
    Ok(transport)
}

async fn bounded<T>(
    operation: impl Future<Output = T>,
    timeout: Duration,
    cancel: &dyn Cancellation,
) -> Result<T, BoundaryError> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(error("cancelled", "MCP request cancelled")),
        result = tokio::time::timeout(timeout, operation) => result.map_err(|_| error("mcp_timeout", "MCP request exceeded its time limit")),
    }
}

fn tool_failure(failure: BoundaryError, attempted: bool) -> BoundaryError {
    if attempted
        && !matches!(
            failure.0.code.as_str(),
            "needs_auth"
                | "mcp_remote_error"
                | "mcp_session_expired"
                | "mcp_not_found"
                | "invalid_args"
                | "credential_scope"
        )
    {
        return error(
            "outcome_unknown",
            "MCP tool completion could not be confirmed. The call was not replayed",
        );
    }
    failure
}

fn service_error(error_value: ServiceError) -> BoundaryError {
    match error_value {
        ServiceError::Timeout { .. } => error("mcp_timeout", "MCP request exceeded its time limit"),
        ServiceError::Cancelled { .. } => error("cancelled", "MCP request cancelled"),
        ServiceError::McpError(_) => {
            error("mcp_remote_error", "MCP server returned a protocol error")
        }
        ServiceError::UnexpectedResponse => {
            error("mcp_protocol", "MCP server returned an unexpected response")
        }
        ServiceError::TransportSend(transport) => transport_error(&transport),
        _ => error(
            "mcp_transport",
            "MCP connection failed. Tool execution was not replayed",
        ),
    }
}

fn connect_error(error_value: ClientInitializeError) -> BoundaryError {
    match error_value {
        ClientInitializeError::TransportError {
            error: transport, ..
        } => transport_error(&transport),
        ClientInitializeError::NoCompatibleProtocolVersion { .. } => error(
            "mcp_protocol_version",
            "MCP server has no supported protocol version",
        ),
        _ => error("mcp_connect", "MCP discovery or initialization failed"),
    }
}

fn transport_error(transport: &rmcp::transport::DynamicTransportError) -> BoundaryError {
    use rmcp::transport::auth::AuthError;
    use rmcp::transport::streamable_http_client::{
        AuthRequiredError, HttpStatusError, InsufficientScopeError, StreamableHttpError,
    };
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(transport.error.as_ref());
    while let Some(source) = current {
        if let Some(io) = source.downcast_ref::<std::io::Error>()
            && let Some(inner) = io.get_ref()
        {
            if inner.is::<wire::OutgoingLimit>() {
                return error(
                    "invalid_args",
                    "MCP outgoing request exceeds its byte limit",
                );
            }
            if inner.is::<http::AudienceMismatch>() {
                return error(
                    "credential_scope",
                    "MCP request target differs from its authorized endpoint",
                );
            }
        }
        if source.is::<AuthRequiredError>() || source.is::<AuthError>() {
            return error("needs_auth", "MCP server requires authorization");
        }
        if source.is::<InsufficientScopeError>() {
            return error(
                "needs_auth",
                "MCP server requires additional authorization scope",
            );
        }
        if let Some(status) = source.downcast_ref::<HttpStatusError>() {
            match status.status {
                401 | 403 => return error("needs_auth", "MCP server rejected its authorization"),
                404 => {
                    return error(
                        "mcp_not_found",
                        "MCP endpoint or session was not found. Tool execution was not replayed",
                    );
                }
                _ => {}
            }
        }
        if matches!(
            source.downcast_ref::<StreamableHttpError<reqwest_mcp::Error>>(),
            Some(StreamableHttpError::SessionExpired)
        ) {
            return error(
                "mcp_session_expired",
                "MCP server session expired. Tool execution was not replayed",
            );
        }
        if let Some(request_error) = source.downcast_ref::<reqwest_mcp::Error>() {
            if matches!(
                request_error.status().map(|status| status.as_u16()),
                Some(401 | 403)
            ) {
                return error("needs_auth", "MCP server rejected its authorization");
            }
            if request_error
                .status()
                .is_some_and(|status| status.as_u16() == 404)
            {
                return error(
                    "mcp_not_found",
                    "MCP endpoint or session was not found. Tool execution was not replayed",
                );
            }
            if request_error.is_connect() {
                return error(
                    "mcp_connect",
                    "MCP HTTP connection could not be established",
                );
            }
            if request_error.is_body() || request_error.is_decode() {
                return error(
                    "mcp_decode",
                    "MCP HTTP response body could not be read or decoded",
                );
            }
        }
        if let Some(http_error) = source.downcast_ref::<StreamableHttpError<reqwest_mcp::Error>>() {
            match http_error {
                StreamableHttpError::UnexpectedServerResponse(_) => {
                    return error(
                        "mcp_http_response",
                        "MCP server returned an invalid HTTP response",
                    );
                }
                StreamableHttpError::UnexpectedContentType(_) => {
                    return error(
                        "mcp_content_type",
                        "MCP server returned an unsupported content type",
                    );
                }
                StreamableHttpError::Deserialize(_) => {
                    return error(
                        "mcp_decode",
                        "MCP response did not match the protocol schema",
                    );
                }
                StreamableHttpError::ResponseBodyTooLarge { .. } => {
                    return error("mcp_limit", "MCP HTTP response exceeds its size limit");
                }
                _ => {}
            }
        }
        current = source.source();
    }
    error(
        "mcp_transport",
        "MCP connection failed. Tool execution was not replayed",
    )
}

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
