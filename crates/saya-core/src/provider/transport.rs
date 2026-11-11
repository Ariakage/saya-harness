//! HTTP and SSE transport shared by model adapters. Provider response bodies
//! and credential values are deliberately excluded from diagnostics.

use crate::{
    boundary::Cancellation,
    config::Environment,
    contract::{BoundaryError, ErrorInfo},
};
use bytes::Buf;
use reqwest::{
    Client, Response, Url,
    header::{HeaderMap, HeaderName, HeaderValue},
    redirect::Policy,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, VecDeque},
    net::IpAddr,
    time::{Duration, SystemTime},
};

const MAX_EVENT_BYTES: usize = 1024 * 1024;
const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;

fn failure(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}

/// Explicit provider authentication. Secret values must come from a trusted
/// configuration source and are never formatted in errors or debug output.
pub enum Authentication {
    Bearer(String),
    ApiKey(String),
    None,
}

/// HTTP transport configuration. Connect and idle timeouts must be nonzero;
/// `total_timeout = 0` disables the client-wide total timeout. Additional
/// headers map names to environment variable names and cannot override
/// protocol, authentication, routing, or framing headers.
pub struct HttpConfig {
    pub base_url: String,
    pub allow_remote_http: bool,
    pub authentication: Authentication,
    pub headers_env: BTreeMap<String, String>,
    pub connect_timeout: Duration,
    pub idle_timeout: Duration,
    pub total_timeout: Duration,
}

/// Reusable client; no provider credentials or response contents are logged.
pub struct HttpTransport {
    client: Client,
    base: Url,
    headers: HeaderMap,
}

fn header(value: &str) -> Result<HeaderValue, BoundaryError> {
    let mut value = HeaderValue::from_str(value)
        .map_err(|_| failure("invalid_config", "invalid HTTP header value"))?;
    value.set_sensitive(true);
    Ok(value)
}

fn reserved_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
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
    )
}

impl HttpTransport {
    /// Validate the endpoint, credentials and extra headers and build a client.
    /// HTTPS uses certificate verification; HTTP requires a literal loopback or
    /// localhost unless explicitly enabled. Redirects are never followed.
    pub fn new(config: HttpConfig) -> Result<Self, BoundaryError> {
        Self::new_with_environment(config, &Environment::capture())
    }

    /// Build using an explicit trusted environment snapshot for extra header
    /// references. Missing, empty or non-UTF-8 references are rejected without
    /// reading the current process environment or disclosing their values.
    pub fn new_with_environment(
        config: HttpConfig,
        environment: &Environment,
    ) -> Result<Self, BoundaryError> {
        if config.connect_timeout.is_zero() || config.idle_timeout.is_zero() {
            return Err(failure(
                "invalid_config",
                "HTTP connect and idle timeouts must be nonzero",
            ));
        }
        let mut base = Url::parse(&config.base_url)
            .map_err(|_| failure("invalid_config", "invalid API base URL"))?;
        if !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(failure(
                "invalid_config",
                "API base URL cannot contain credentials, query or fragment",
            ));
        }
        let host = base
            .host_str()
            .ok_or_else(|| failure("invalid_config", "API base URL requires a host"))?;
        let loopback = host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        match base.scheme() {
            "https" => {}
            "http" if loopback || config.allow_remote_http => {}
            "http" => {
                return Err(failure(
                    "invalid_config",
                    "remote HTTP requires explicit opt-in",
                ));
            }
            _ => {
                return Err(failure(
                    "invalid_config",
                    "API base URL must use HTTP or HTTPS",
                ));
            }
        }
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }
        let mut headers = HeaderMap::new();
        match config.authentication {
            Authentication::Bearer(secret) if !secret.is_empty() => {
                headers.insert(
                    reqwest::header::AUTHORIZATION,
                    header(&format!("Bearer {secret}"))?,
                );
            }
            Authentication::ApiKey(secret) if !secret.is_empty() => {
                headers.insert("x-api-key", header(&secret)?);
            }
            Authentication::None => {}
            _ => return Err(failure("invalid_config", "authentication secret is empty")),
        }
        for (name, env_name) in config.headers_env {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| failure("invalid_config", "invalid HTTP header name"))?;
            if reserved_header(&name) || headers.contains_key(&name) {
                return Err(failure(
                    "invalid_config",
                    "reserved or duplicate HTTP header",
                ));
            }
            let value = environment
                .get_os(&env_name)
                .and_then(|value| value.to_str())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    failure(
                        "invalid_config",
                        "HTTP header environment reference is unavailable",
                    )
                })?;
            headers.insert(name, header(value)?);
        }
        let mut client = Client::builder()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(config.connect_timeout)
            .read_timeout(config.idle_timeout);
        if !config.total_timeout.is_zero() {
            client = client.timeout(config.total_timeout);
        }
        let client = client
            .build()
            .map_err(|_| failure("invalid_config", "could not build HTTP client"))?;
        Ok(Self {
            client,
            base,
            headers,
        })
    }

    /// POST JSON to one provider endpoint. The endpoint is a fixed relative
    /// path, not model-provided input. No retries are performed here.
    pub async fn post(
        &self,
        endpoint: &str,
        body: &Value,
        version: Option<&str>,
        cancel: &dyn Cancellation,
    ) -> Result<HttpResponse, BoundaryError> {
        if cancel.is_cancelled() {
            return Err(failure("cancelled", "request cancelled"));
        }
        if endpoint.is_empty()
            || endpoint.starts_with('/')
            || endpoint
                .split('/')
                .any(|s| s.is_empty() || s == "." || s == "..")
            || endpoint.contains(['?', '#', ':', '%', '\\'])
        {
            return Err(failure("invalid_config", "invalid provider endpoint"));
        }
        let url = self
            .base
            .join(endpoint)
            .map_err(|_| failure("invalid_config", "invalid provider endpoint"))?;
        let mut req = self
            .client
            .post(url)
            .headers(self.headers.clone())
            .header(
                reqwest::header::ACCEPT,
                "text/event-stream, application/json",
            )
            .json(body);
        if let Some(version) = version {
            req = req.header("anthropic-version", header(version)?);
        }
        let response = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(failure("cancelled", "request cancelled")),
            result = req.send() => result.map_err(map_transport_error)?,
        };
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let code = match status {
                401 | 403 => "authentication_error",
                429 => "rate_limited",
                500 | 502 | 503 | 504 | 529 => "server_error",
                400..=499 => "provider_request_error",
                _ => "provider_error",
            };
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_retry_after);
            return Err(BoundaryError(
                ErrorInfo {
                    code: code.into(),
                    message: format!("provider returned HTTP {status}"),
                },
                retry_after,
                None,
            ));
        }
        Ok(HttpResponse { response })
    }
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    httpdate::parse_http_date(value)
        .ok()?
        .duration_since(SystemTime::now())
        .ok()
}

fn map_transport_error(error: reqwest::Error) -> BoundaryError {
    if error.is_timeout() {
        failure("model_timeout", "provider request timed out")
    } else if error.is_connect() {
        failure("transient_connection", "provider connection failed")
    } else {
        failure("provider_transport_error", "provider transport failed")
    }
}

/// Successful response whose body is consumed with bounded buffering.
pub struct HttpResponse {
    response: Response,
}
impl HttpResponse {
    /// Read a non-streaming JSON response, observing cancellation and a size cap.
    pub async fn json(mut self, cancel: &dyn Cancellation) -> Result<Value, BoundaryError> {
        let mut body = Vec::new();
        while let Some(chunk) = next_chunk(&mut self.response, cancel).await? {
            if body.len().saturating_add(chunk.len()) > MAX_JSON_BYTES {
                return Err(failure(
                    "invalid_model_response",
                    "provider response exceeds size limit",
                ));
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body)
            .map_err(|_| failure("invalid_model_response", "provider returned invalid JSON"))
    }

    /// Convert a successful SSE response into an incremental event reader.
    pub fn events(self) -> Result<EventStream, BoundaryError> {
        let is_sse = self
            .response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.split(';')
                    .next()
                    .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
            });
        if !is_sse {
            return Err(failure(
                "invalid_model_response",
                "provider response is not an event stream",
            ));
        }
        Ok(EventStream {
            response: self.response,
            decoder: SseDecoder::default(),
            pending: bytes::Bytes::new(),
            ended: false,
        })
    }
}

async fn next_chunk(
    response: &mut Response,
    cancel: &dyn Cancellation,
) -> Result<Option<bytes::Bytes>, BoundaryError> {
    if cancel.is_cancelled() {
        return Err(failure("cancelled", "request cancelled"));
    }
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(failure("cancelled", "request cancelled")),
        result = response.chunk() => result.map_err(map_transport_error),
    }
}

/// One SSE event. Multiple data lines are joined by a newline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

/// Incremental event stream; EOF is not a provider completion signal.
pub struct EventStream {
    response: Response,
    decoder: SseDecoder,
    pending: bytes::Bytes,
    ended: bool,
}
impl EventStream {
    pub async fn next(
        &mut self,
        cancel: &dyn Cancellation,
    ) -> Result<Option<SseEvent>, BoundaryError> {
        loop {
            if cancel.is_cancelled() {
                return Err(failure("cancelled", "request cancelled"));
            }
            if let Some(event) = self.decoder.events.pop_front() {
                return Ok(Some(event));
            }
            if !self.pending.is_empty() {
                let consumed = self.decoder.feed(&self.pending)?;
                self.pending.advance(consumed);
                continue;
            }
            if self.ended {
                return Ok(None);
            }
            match next_chunk(&mut self.response, cancel).await? {
                Some(bytes) => self.pending = bytes,
                None => self.ended = true,
            }
        }
    }
}

#[derive(Default)]
struct SseDecoder {
    line: Vec<u8>,
    data: String,
    event: String,
    has_data: bool,
    first_line: bool,
    after_cr: bool,
    events: VecDeque<SseEvent>,
}
impl SseDecoder {
    fn feed(&mut self, bytes: &[u8]) -> Result<usize, BoundaryError> {
        for (index, &byte) in bytes.iter().enumerate() {
            if self.after_cr {
                self.after_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\r' => {
                    self.process_line()?;
                    self.after_cr = true;
                }
                b'\n' => self.process_line()?,
                _ => {
                    self.line.push(byte);
                    if self
                        .line
                        .len()
                        .saturating_add(self.data.len())
                        .saturating_add(self.event.len())
                        > MAX_EVENT_BYTES
                    {
                        return Err(failure(
                            "invalid_model_response",
                            "provider event exceeds size limit",
                        ));
                    }
                }
            }
            if !self.events.is_empty() {
                return Ok(index + 1);
            }
        }
        Ok(bytes.len())
    }
    fn process_line(&mut self) -> Result<(), BoundaryError> {
        let raw = std::mem::take(&mut self.line);
        let mut line = std::str::from_utf8(&raw)
            .map_err(|_| failure("invalid_model_response", "provider event is not UTF-8"))?;
        if !self.first_line {
            self.first_line = true;
            line = line.strip_prefix('\u{feff}').unwrap_or(line);
        }
        if line.is_empty() {
            if self.has_data {
                self.data.pop();
                self.events.push_back(SseEvent {
                    event: std::mem::take(&mut self.event),
                    data: std::mem::take(&mut self.data),
                });
                self.has_data = false;
            } else {
                self.event.clear();
            }
        } else if !line.starts_with(':') {
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "data" => {
                    self.has_data = true;
                    self.data.push_str(value);
                    self.data.push('\n');
                }
                "event" => {
                    self.event.clear();
                    self.event.push_str(value);
                }
                _ => {}
            }
        }
        Ok(())
    }
}
