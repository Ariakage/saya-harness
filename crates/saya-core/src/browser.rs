//! Host-owned browser capability boundary.
//!
//! `BrowserProvider` is the only way the runtime reaches a browser worker.
//! `BrowserState` isolates one private session per Run, serializes actions per
//! page, and maps provider element keys to random opaque model-visible
//! references. It never exposes selectors, scripts, evaluate, install, raw MCP
//! discovery or developer tools. Unknown dispatched actions stop replay until a
//! fresh snapshot confirms an observable state.
use crate::{
    boundary::{Cancellation, Operation, Tool, ToolContext, ToolDefinition},
    contract::{BoundaryError, ErrorInfo, Run, RunId, SessionId, ToolCall, ToolResult},
    dispatch::ToolDispatcher,
    output_store::{
        CaptureCompletion, CapturedToolResult, OutputMetadata, OutputScope, OutputStore,
        OutputStream, SealedOutput,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::sync::Mutex as AsyncMutex;

const DEFAULT_MAX_SNAPSHOT_BYTES: usize = 256 * 1024;
const DEFAULT_MAX_WAIT_MS: u64 = 60_000;
const MAX_PAGES: usize = 16;
const MAX_ELEMENTS: usize = 512;
const MAX_ELEMENT_BYTES: usize = 128 * 1024;
const MAX_FILL_BYTES: usize = 64 * 1024;
const INLINE_SNAPSHOT_BYTES: usize = 16 * 1024;
const MAX_PROVIDER_URL_BYTES: usize = 8192;
const MAX_PROVIDER_TITLE_BYTES: usize = 4096;
const MAX_PROVIDER_DETAIL_BYTES: usize = 4096;
const MAX_PROVIDER_TOKEN_BYTES: usize = 256;
const MAX_PROVIDER_KEY_BYTES: usize = 1024;
const MAX_PROVIDER_FILENAME_BYTES: usize = 255;
const MAX_PROVIDER_MIME_BYTES: usize = 128;
const UNTRUSTED_BROWSER: &str = "untrusted_browser";

fn invalid_provider_result(message: &'static str) -> BrowserFailure {
    BrowserFailure::new("invalid_provider_result", message)
}

/// Validate provider-supplied provenance fields. These values become the
/// model-visible source identity, so oversize or control-bearing values fail
/// closed instead of being silently truncated and presented as authoritative.
fn provider_text(
    value: &str,
    max_bytes: usize,
    message: &'static str,
) -> Result<(), BrowserFailure> {
    if value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(invalid_provider_result(message));
    }
    Ok(())
}

fn provider_field(
    value: &str,
    max_bytes: usize,
    message: &'static str,
) -> Result<(), BrowserFailure> {
    provider_text(value, max_bytes, message)?;
    if value.is_empty() {
        return Err(invalid_provider_result(message));
    }
    Ok(())
}

fn provider_url(value: &str) -> Result<(), BrowserFailure> {
    provider_field(
        value,
        MAX_PROVIDER_URL_BYTES,
        "Browser provider returned an invalid URL",
    )?;
    reqwest::Url::parse(value)
        .map(|_| ())
        .map_err(|_| invalid_provider_result("Browser provider returned an invalid URL"))
}

fn provider_optional_url(value: Option<&str>) -> Result<(), BrowserFailure> {
    value.map_or(Ok(()), provider_url)
}

fn provider_optional_detail(value: Option<&str>) -> Result<(), BrowserFailure> {
    value.map_or(Ok(()), |value| {
        provider_text(
            value,
            MAX_PROVIDER_DETAIL_BYTES,
            "Browser provider returned an invalid action detail",
        )
    })
}

fn failure(code: &str, message: impl Into<String>) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}

fn invalid_config(message: &'static str) -> BoundaryError {
    failure("invalid_config", message)
}

/// Default-disabled browser settings. A real worker and profile policy arrive
/// in DEV-080; this stage validates configuration and exposes explicit error
/// boundaries instead of pretending a browser is available.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrowserSettings {
    pub enabled: bool,
    pub max_snapshot_bytes: usize,
    pub max_wait_ms: u64,
}
impl Default for BrowserSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            max_snapshot_bytes: DEFAULT_MAX_SNAPSHOT_BYTES,
            max_wait_ms: DEFAULT_MAX_WAIT_MS,
        }
    }
}
impl BrowserSettings {
    pub fn validate(&self) -> Result<(), BoundaryError> {
        if self.max_snapshot_bytes == 0
            || self.max_snapshot_bytes > 4 * 1024 * 1024
            || self.max_wait_ms == 0
            || self.max_wait_ms > 10 * 60 * 1000
        {
            return Err(invalid_config("Browser limits are invalid"));
        }
        Ok(())
    }
}

/// Capabilities declared by a host-selected provider. Screenshot, download and
/// upload tools are registered only when the provider declares support.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserFacts {
    pub provider: String,
    pub screenshots: bool,
    pub downloads: bool,
    pub uploads: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserFailure {
    pub code: String,
    pub message: String,
    /// The provider may have performed part of an action without a terminal
    /// result. The session stops replay until a snapshot confirms state.
    pub uncertain: bool,
}
impl BrowserFailure {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            uncertain: false,
        }
    }
    pub fn uncertain(message: impl Into<String>) -> Self {
        Self {
            code: "outcome_unknown".into(),
            message: message.into(),
            uncertain: true,
        }
    }
    pub fn unsupported(operation: &str) -> Self {
        Self::new(
            "unsupported",
            format!("Browser provider does not support {operation}"),
        )
    }
    fn into_boundary(self) -> BoundaryError {
        failure(&self.code, self.message)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserElement {
    pub key: String,
    pub role: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserSnapshot {
    pub url: String,
    pub title: Option<String>,
    pub text: String,
    pub elements: Vec<BrowserElement>,
    /// The provider itself stopped at an internal size or extraction limit.
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserOpened {
    pub session: String,
    pub page: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserAction {
    pub url: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BrowserScreenshot {
    pub mime: String,
    pub filename: String,
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone)]
pub struct BrowserDownload {
    pub filename: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct BrowserUpload {
    pub filename: String,
    pub bytes_sent: u64,
}

/// One host-selected browser backend. Implementations must terminate on
/// cancellation and must not expose a broader tool surface than this trait.
#[async_trait::async_trait]
pub trait BrowserProvider: Send + Sync {
    fn facts(&self) -> BrowserFacts;

    async fn open(
        &self,
        session: Option<&str>,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserOpened, BrowserFailure>;
    async fn close(&self, session: &str, cancel: &dyn Cancellation) -> Result<(), BrowserFailure>;
    async fn navigate(
        &self,
        session: &str,
        page: &str,
        url: &str,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserAction, BrowserFailure>;
    async fn snapshot(
        &self,
        session: &str,
        page: &str,
        max_bytes: usize,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserSnapshot, BrowserFailure>;
    async fn click(
        &self,
        session: &str,
        page: &str,
        element: &str,
        button: Option<&str>,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserAction, BrowserFailure>;
    async fn fill(
        &self,
        session: &str,
        page: &str,
        element: &str,
        text: &str,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserAction, BrowserFailure>;
    async fn press(
        &self,
        session: &str,
        page: &str,
        key: &str,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserAction, BrowserFailure>;
    async fn wait(
        &self,
        session: &str,
        page: &str,
        milliseconds: u64,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserAction, BrowserFailure>;
    async fn screenshot(
        &self,
        _session: &str,
        _page: &str,
        _cancel: &dyn Cancellation,
    ) -> Result<BrowserScreenshot, BrowserFailure> {
        Err(BrowserFailure::unsupported("screenshot"))
    }
    async fn download(
        &self,
        _session: &str,
        _page: &str,
        _cancel: &dyn Cancellation,
    ) -> Result<BrowserDownload, BrowserFailure> {
        Err(BrowserFailure::unsupported("download"))
    }
    async fn upload(
        &self,
        _session: &str,
        _page: &str,
        _element: &str,
        _artifact: &str,
        _cancel: &dyn Cancellation,
    ) -> Result<BrowserUpload, BrowserFailure> {
        Err(BrowserFailure::unsupported("upload"))
    }
    async fn shutdown(&self, _cancel: &dyn Cancellation) -> Result<(), BrowserFailure>;
}

#[derive(Default)]
struct PageDocument {
    token: Option<String>,
    elements: BTreeMap<String, String>,
    revision: u64,
    /// An action on this page may have been dispatched without a confirmed
    /// outcome. Only a fresh snapshot of this page, or a complete close, clears
    /// it. It must never be cleared by another page's snapshot or a new open.
    uncertain: bool,
}
impl PageDocument {
    fn invalidate(&mut self) {
        self.token = None;
        self.elements.clear();
        self.revision = self.revision.saturating_add(1);
    }
    fn fail_unknown(&mut self) {
        self.uncertain = true;
        self.invalidate();
    }
    fn observe(&mut self, token: String, elements: BTreeMap<String, String>) {
        self.uncertain = false;
        self.token = Some(token);
        self.elements = elements;
        self.revision = self.revision.saturating_add(1);
    }
}

struct Page {
    key: String,
    document: AsyncMutex<PageDocument>,
}

struct Inner {
    session: Option<String>,
    pages: BTreeMap<String, Arc<Page>>,
    /// An `open` call may have created provider-side resources without a
    /// confirmed result. This is a global conservative state: it blocks all
    /// old pages and can only be cleared by a complete close or shutdown.
    open_uncertain: bool,
    /// The provider has completed its full shutdown. A later explicit `open`
    /// starts a brand-new provider resource lifecycle.
    provider_shutdown: bool,
    closed: bool,
}

/// One Run's private browser session state. Cheap to share between adapters;
/// the provider itself remains host-owned.
pub struct BrowserState {
    inner: AsyncMutex<Inner>,
    shutdown_lock: AsyncMutex<()>,
    provider: Arc<dyn BrowserProvider>,
    facts: BrowserFacts,
    settings: BrowserSettings,
    run_id: RunId,
    session_id: SessionId,
}
impl std::fmt::Debug for BrowserState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserState")
            .field("run_id", &self.run_id)
            .field("session_id", &self.session_id)
            .field("facts", &self.facts)
            .finish_non_exhaustive()
    }
}

/// Model-visible page and document views. Page and document identities are
/// opaque host tokens; provider keys are never exposed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenView {
    pub page: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NavigateView {
    pub page: String,
    pub url: String,
    pub document_stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElementView {
    pub element_ref: String,
    pub role: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotView {
    pub page: String,
    pub document: String,
    pub document_revision: u64,
    pub url: String,
    pub title: Option<String>,
    pub text: String,
    pub elements: Vec<ElementView>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionResultView {
    pub page: String,
    pub url: Option<String>,
    pub detail: Option<String>,
    pub document_stale: bool,
    pub document_revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotView {
    pub mime: String,
    pub filename: String,
    pub width: u32,
    pub height: u32,
    pub bytes: usize,
    pub sha256: String,
    #[serde(skip)]
    pub raw_bytes: Vec<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_ref: Option<String>,
    pub model_image_attached: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadView {
    pub filename: String,
    pub mime: String,
    pub bytes: usize,
    pub sha256: String,
    pub artifact_stored: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadView {
    pub filename: String,
    pub bytes_sent: u64,
    pub document_revision: u64,
}

fn opaque(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::now_v7().simple())
}

fn digest_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn bounded_text(text: &str, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text.to_owned(), false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

/// Page content is untrusted data, so control characters are replaced rather
/// than rejected; the replacement is reported as truncation because the
/// original text could not be represented verbatim.
fn bounded_content(text: &str, limit: usize) -> (String, bool) {
    let (bounded, mut truncated) = bounded_text(text, limit);
    if bounded.chars().any(char::is_control) {
        truncated = true;
        (
            bounded
                .chars()
                .map(|character| {
                    if character.is_control() {
                        ' '
                    } else {
                        character
                    }
                })
                .collect(),
            truncated,
        )
    } else {
        (bounded, truncated)
    }
}

fn valid_token(value: &str, prefix: &str) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|suffix| !suffix.is_empty() && suffix.len() <= 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

impl BrowserState {
    pub fn new(
        settings: BrowserSettings,
        provider: Arc<dyn BrowserProvider>,
        run: Run,
    ) -> Result<Self, BoundaryError> {
        settings.validate()?;
        if !settings.enabled {
            return Err(invalid_config("Browser capability is disabled"));
        }
        let facts = provider.facts();
        if facts.provider.trim().is_empty() || facts.provider.len() > 64 {
            return Err(invalid_config("Browser provider identity is invalid"));
        }
        Ok(Self {
            inner: AsyncMutex::new(Inner {
                session: None,
                pages: BTreeMap::new(),
                open_uncertain: false,
                provider_shutdown: false,
                closed: false,
            }),
            shutdown_lock: AsyncMutex::new(()),
            provider,
            facts,
            settings,
            run_id: run.id,
            session_id: run.session_id,
        })
    }

    pub fn facts(&self) -> &BrowserFacts {
        &self.facts
    }

    fn verify(&self, context: &ToolContext) -> Result<(), BoundaryError> {
        if context.run.id != self.run_id || context.run.session_id != self.session_id {
            return Err(failure(
                "invalid_run",
                "Browser state does not belong to this Run and Session",
            ));
        }
        Ok(())
    }

    /// Open a new page in the Run-private session. A successful open is a
    /// fresh observation point for the new page only: it never clears an
    /// unknown state belonging to another page. If the provider was shut down,
    /// a new open starts a brand-new provider resource lifecycle.
    pub async fn open(
        &self,
        context: &ToolContext,
        cancel: &dyn Cancellation,
    ) -> Result<OpenView, BoundaryError> {
        self.verify(context)?;
        // Cancellation observed before dispatch cannot have created provider
        // resources and stays a plain `cancelled` outcome.
        if cancel.is_cancelled() {
            return Err(failure("cancelled", "Browser open cancelled"));
        }
        let session = {
            let mut inner = self.inner.lock().await;
            if inner.open_uncertain {
                return Err(failure(
                    "outcome_unknown",
                    "Browser open outcome is unknown; close or shut down the session",
                ));
            }
            if inner.pages.len() >= MAX_PAGES {
                return Err(failure(
                    "browser_limit",
                    "Browser page limit was reached; close the Run session first",
                ));
            }
            // Any dispatched open starts a new lifecycle: a previous complete
            // shutdown no longer proves the provider is clean if this attempt
            // fails or is cancelled.
            inner.provider_shutdown = false;
            inner.session.clone()
        };
        let opened = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                let mut inner = self.inner.lock().await;
                inner.open_uncertain = true;
                inner.provider_shutdown = false;
                return Err(BrowserFailure::uncertain(
                    "Browser open was cancelled after dispatch; the provider may hold resources",
                )
                .into_boundary());
            }
            result = self.provider.open(session.as_deref(), cancel) => result,
        };
        match opened {
            Ok(opened) => {
                if let Err(error) = provider_field(
                    &opened.session,
                    MAX_PROVIDER_TOKEN_BYTES,
                    "Browser provider returned an invalid session identity",
                )
                .and_then(|()| {
                    provider_field(
                        &opened.page,
                        MAX_PROVIDER_TOKEN_BYTES,
                        "Browser provider returned an invalid page identity",
                    )
                })
                .and_then(|()| provider_optional_url(opened.url.as_deref()))
                {
                    self.inner.lock().await.open_uncertain = true;
                    return Err(error.into_boundary());
                }
                let page = opaque("page");
                let mut inner = self.inner.lock().await;
                if inner.open_uncertain {
                    return Err(failure(
                        "outcome_unknown",
                        "Browser open outcome is unknown; close or shut down the session",
                    ));
                }
                inner.session = Some(opened.session);
                inner.provider_shutdown = false;
                inner.closed = false;
                inner.pages.insert(
                    page.clone(),
                    Arc::new(Page {
                        key: opened.page,
                        document: AsyncMutex::new(PageDocument::default()),
                    }),
                );
                Ok(OpenView {
                    page,
                    url: opened.url,
                })
            }
            Err(error) => {
                if error.uncertain {
                    self.inner.lock().await.open_uncertain = true;
                }
                Err(error.into_boundary())
            }
        }
    }

    /// Close the Run-private session and all pages. Provider confirmation is
    /// required before any session or page state is discarded. A known or
    /// unknown failure leaves the state in place so the caller can safely retry
    /// close; a second call never reports success without provider agreement.
    pub async fn close(
        &self,
        context: &ToolContext,
        cancel: &dyn Cancellation,
    ) -> Result<(), BoundaryError> {
        self.verify(context)?;
        let (session, pages, open_uncertain, provider_shutdown) = {
            let inner = self.inner.lock().await;
            (
                inner.session.clone(),
                inner.pages.values().cloned().collect::<Vec<_>>(),
                inner.open_uncertain,
                inner.provider_shutdown,
            )
        };
        if session.is_none() && !open_uncertain && provider_shutdown {
            return Ok(());
        }
        let mut guards = Vec::with_capacity(pages.len());
        for page in &pages {
            guards.push(page.document.lock().await);
        }
        let result = match session.as_deref() {
            Some(session) => self.provider.close(session, cancel).await,
            // An unknown open may have created provider resources with no
            // confirmed session identity. Only a full provider shutdown can
            // establish that the provider is clean.
            None if open_uncertain => self.provider.shutdown(cancel).await,
            None => Ok(()),
        };
        drop(guards);
        match result {
            Ok(()) => {
                let mut inner = self.inner.lock().await;
                if session
                    .as_deref()
                    .is_some_and(|session| inner.session.as_deref() != Some(session))
                {
                    return Err(failure(
                        "outcome_unknown",
                        "Browser session changed during close",
                    ));
                }
                inner.session = None;
                inner.pages.clear();
                inner.open_uncertain = false;
                inner.closed = true;
                inner.provider_shutdown = session.is_none() && open_uncertain;
                Ok(())
            }
            Err(error) => {
                if error.uncertain {
                    self.inner.lock().await.open_uncertain = true;
                }
                Err(error.into_boundary())
            }
        }
    }

    async fn active_page(
        &self,
        context: &ToolContext,
        page: &str,
    ) -> Result<(String, Arc<Page>), BoundaryError> {
        self.verify(context)?;
        let inner = self.inner.lock().await;
        if inner.open_uncertain {
            return Err(failure(
                "outcome_unknown",
                "Browser open outcome is unknown; close or shut down the session",
            ));
        }
        if inner.closed {
            return Err(failure("browser_closed", "Browser session is closed"));
        }
        let session = inner
            .session
            .clone()
            .ok_or_else(|| failure("browser_closed", "Browser session is not open"))?;
        let page = inner.pages.get(page).cloned().ok_or_else(|| {
            failure(
                "invalid_page",
                "Browser page reference is unknown or closed",
            )
        })?;
        Ok((session, page))
    }

    /// Navigate one page. Existing document references are conservatively
    /// invalidated, and an unknown provider result binds to this page only.
    pub async fn navigate(
        &self,
        context: &ToolContext,
        page: &str,
        url: &str,
        cancel: &dyn Cancellation,
    ) -> Result<NavigateView, BoundaryError> {
        crate::network::normalize_target(url).map_err(|_| {
            failure(
                "invalid_args",
                "Browser navigation requires an explicit HTTP(S) URL",
            )
        })?;
        let page_id = page.to_owned();
        let (session, page) = self.active_page(context, page).await?;
        if cancel.is_cancelled() {
            return Err(failure("cancelled", "Browser navigation cancelled"));
        }
        let mut document = page.document.lock().await;
        if document.uncertain {
            return Err(failure(
                "outcome_unknown",
                "This page's outcome is unknown; take a fresh snapshot first",
            ));
        }
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                document.fail_unknown();
                return Err(BrowserFailure::uncertain(
                    "Browser navigation was cancelled after dispatch",
                )
                .into_boundary());
            }
            result = self.provider.navigate(&session, &page.key, url, cancel) => result,
        };
        document.invalidate();
        drop(document);
        match result {
            Ok(action) => {
                if let Err(error) = provider_optional_url(action.url.as_deref())
                    .and_then(|()| provider_optional_detail(action.detail.as_deref()))
                {
                    return Err(error.into_boundary());
                }
                Ok(NavigateView {
                    page: page_id,
                    url: action.url.unwrap_or_else(|| url.to_owned()),
                    document_stale: true,
                })
            }
            Err(error) => {
                if error.uncertain {
                    let mut document = page.document.lock().await;
                    document.fail_unknown();
                }
                Err(error.into_boundary())
            }
        }
    }

    /// Take a fresh observation. A successful snapshot replaces this page's
    /// document token and all element references and clears this page's own
    /// unknown state only.
    pub async fn snapshot(
        &self,
        context: &ToolContext,
        page: &str,
        cancel: &dyn Cancellation,
    ) -> Result<SnapshotView, BoundaryError> {
        let page_id = page.to_owned();
        let (session, page) = self.active_page(context, page).await?;
        let mut document = page.document.lock().await;
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(failure("cancelled", "Browser snapshot cancelled")),
            result = self
                .provider
                .snapshot(&session, &page.key, self.settings.max_snapshot_bytes, cancel) => result,
        };
        let snapshot = match result {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if error.uncertain {
                    document.fail_unknown();
                }
                return Err(error.into_boundary());
            }
        };
        if let Err(error) = provider_url(&snapshot.url) {
            return Err(error.into_boundary());
        }
        if let Some(title) = snapshot.title.as_deref()
            && let Err(error) = provider_text(
                title,
                MAX_PROVIDER_TITLE_BYTES,
                "Browser provider returned an invalid title",
            )
        {
            return Err(error.into_boundary());
        }
        let (text, text_truncated) = bounded_text(&snapshot.text, self.settings.max_snapshot_bytes);
        let token = opaque("doc");
        let mut elements = Vec::new();
        let mut element_map = BTreeMap::new();
        let mut truncated = snapshot.truncated || text_truncated;
        let mut element_bytes = 0usize;
        for element in snapshot.elements.iter().take(MAX_ELEMENTS) {
            if let Err(error) = provider_field(
                &element.key,
                MAX_PROVIDER_KEY_BYTES,
                "Browser provider returned an invalid element identity",
            ) {
                return Err(error.into_boundary());
            }
            let reference = opaque("el");
            let (name, name_truncated) = bounded_content(&element.name, 1024);
            let (role, role_truncated) = bounded_content(&element.role, 128);
            let size = name.len().saturating_add(role.len()).saturating_add(64);
            if element_bytes.saturating_add(size) > MAX_ELEMENT_BYTES {
                truncated = true;
                break;
            }
            element_bytes += size;
            truncated |= name_truncated || role_truncated;
            element_map.insert(reference.clone(), element.key.clone());
            elements.push(ElementView {
                element_ref: reference,
                role,
                name,
            });
        }
        if snapshot.elements.len() > elements.len() {
            truncated = true;
        }
        document.observe(token.clone(), element_map);
        let revision = document.revision;
        drop(document);
        Ok(SnapshotView {
            page: page_id,
            document: token,
            document_revision: revision,
            url: snapshot.url,
            title: snapshot.title,
            text,
            elements,
            truncated,
        })
    }

    fn verify_document(
        document: &PageDocument,
        token: &str,
        element: &str,
    ) -> Result<String, BoundaryError> {
        if document.uncertain {
            return Err(failure(
                "outcome_unknown",
                "This page's outcome is unknown; take a fresh snapshot first",
            ));
        }
        if document.token.as_deref() != Some(token) {
            return Err(failure(
                "stale_element",
                "Document token is stale; take a new snapshot",
            ));
        }
        document.elements.get(element).cloned().ok_or_else(|| {
            failure(
                "stale_element",
                "Element reference is stale; take a new snapshot",
            )
        })
    }

    pub async fn click(
        &self,
        context: &ToolContext,
        page: &str,
        document: &str,
        element: &str,
        button: Option<&str>,
        cancel: &dyn Cancellation,
    ) -> Result<ActionResultView, BoundaryError> {
        let page_id = page.to_owned();
        let (session, page_state) = self.active_page(context, page).await?;
        let mut state = page_state.document.lock().await;
        let provider_element = Self::verify_document(&state, document, element)?;
        if cancel.is_cancelled() {
            return Err(failure("cancelled", "Browser click cancelled"));
        }
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                state.fail_unknown();
                return Err(BrowserFailure::uncertain(
                    "Browser click was cancelled after dispatch",
                )
                .into_boundary());
            }
            result = self
                .provider
                .click(&session, &page_state.key, &provider_element, button, cancel) => result,
        };
        state.invalidate();
        let revision = state.revision;
        drop(state);
        match result {
            Ok(action) => {
                if let Err(error) = provider_optional_url(action.url.as_deref())
                    .and_then(|()| provider_optional_detail(action.detail.as_deref()))
                {
                    page_state.document.lock().await.fail_unknown();
                    return Err(error.into_boundary());
                }
                Ok(ActionResultView {
                    page: page_id,
                    url: action.url,
                    detail: action.detail,
                    document_stale: true,
                    document_revision: revision,
                })
            }
            Err(error) => {
                if error.uncertain {
                    page_state.document.lock().await.fail_unknown();
                }
                Err(error.into_boundary())
            }
        }
    }

    pub async fn fill(
        &self,
        context: &ToolContext,
        page: &str,
        document: &str,
        element: &str,
        text: &str,
        cancel: &dyn Cancellation,
    ) -> Result<ActionResultView, BoundaryError> {
        let page_id = page.to_owned();
        let (session, page_state) = self.active_page(context, page).await?;
        let mut state = page_state.document.lock().await;
        let provider_element = Self::verify_document(&state, document, element)?;
        if cancel.is_cancelled() {
            return Err(failure("cancelled", "Browser fill cancelled"));
        }
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                state.fail_unknown();
                return Err(BrowserFailure::uncertain(
                    "Browser fill was cancelled after dispatch",
                )
                .into_boundary());
            }
            result = self
                .provider
                .fill(&session, &page_state.key, &provider_element, text, cancel) => result,
        };
        state.invalidate();
        let revision = state.revision;
        drop(state);
        match result {
            Ok(action) => {
                if let Err(error) = provider_optional_url(action.url.as_deref())
                    .and_then(|()| provider_optional_detail(action.detail.as_deref()))
                {
                    page_state.document.lock().await.fail_unknown();
                    return Err(error.into_boundary());
                }
                Ok(ActionResultView {
                    page: page_id,
                    url: action.url,
                    detail: action.detail,
                    document_stale: true,
                    document_revision: revision,
                })
            }
            Err(error) => {
                if error.uncertain {
                    page_state.document.lock().await.fail_unknown();
                }
                Err(error.into_boundary())
            }
        }
    }

    pub async fn press(
        &self,
        context: &ToolContext,
        page: &str,
        document: &str,
        key: &str,
        cancel: &dyn Cancellation,
    ) -> Result<ActionResultView, BoundaryError> {
        let page_id = page.to_owned();
        let (session, page) = self.active_page(context, page).await?;
        let mut state = page.document.lock().await;
        if state.uncertain {
            return Err(failure(
                "outcome_unknown",
                "This page's outcome is unknown; take a fresh snapshot first",
            ));
        }
        if state.token.as_deref() != Some(document) {
            return Err(failure(
                "stale_element",
                "Document token is stale; take a new snapshot",
            ));
        }
        if cancel.is_cancelled() {
            return Err(failure("cancelled", "Browser press cancelled"));
        }
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                state.fail_unknown();
                return Err(BrowserFailure::uncertain(
                    "Browser press was cancelled after dispatch",
                )
                .into_boundary());
            }
            result = self.provider.press(&session, &page.key, key, cancel) => result,
        };
        state.invalidate();
        let revision = state.revision;
        drop(state);
        match result {
            Ok(action) => {
                if let Err(error) = provider_optional_url(action.url.as_deref())
                    .and_then(|()| provider_optional_detail(action.detail.as_deref()))
                {
                    page.document.lock().await.fail_unknown();
                    return Err(error.into_boundary());
                }
                Ok(ActionResultView {
                    page: page_id,
                    url: action.url,
                    detail: action.detail,
                    document_stale: true,
                    document_revision: revision,
                })
            }
            Err(error) => {
                if error.uncertain {
                    page.document.lock().await.fail_unknown();
                }
                Err(error.into_boundary())
            }
        }
    }

    pub async fn wait(
        &self,
        context: &ToolContext,
        page: &str,
        milliseconds: u64,
        cancel: &dyn Cancellation,
    ) -> Result<ActionResultView, BoundaryError> {
        if milliseconds > self.settings.max_wait_ms {
            return Err(failure(
                "invalid_args",
                "Browser wait exceeds the configured limit",
            ));
        }
        let page_id = page.to_owned();
        let (session, page) = self.active_page(context, page).await?;
        let mut state = page.document.lock().await;
        if state.uncertain {
            return Err(failure(
                "outcome_unknown",
                "This page's outcome is unknown; take a fresh snapshot first",
            ));
        }
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                state.invalidate();
                return Err(failure("cancelled", "Browser wait cancelled"));
            }
            result = self.provider.wait(&session, &page.key, milliseconds, cancel) => result,
        };
        // Waiting never confirms a document revision: conservatively invalidate.
        state.invalidate();
        let revision = state.revision;
        drop(state);
        match result {
            Ok(action) => {
                provider_optional_url(action.url.as_deref())
                    .and_then(|()| provider_optional_detail(action.detail.as_deref()))
                    .map_err(BrowserFailure::into_boundary)?;
                Ok(ActionResultView {
                    page: page_id,
                    url: action.url,
                    detail: action.detail,
                    document_stale: true,
                    document_revision: revision,
                })
            }
            Err(error) => {
                if error.uncertain {
                    page.document.lock().await.fail_unknown();
                }
                Err(error.into_boundary())
            }
        }
    }

    pub async fn screenshot(
        &self,
        context: &ToolContext,
        page: &str,
        cancel: &dyn Cancellation,
    ) -> Result<ScreenshotView, BoundaryError> {
        let (session, page) = self.active_page(context, page).await?;
        let mut state = page.document.lock().await;
        if state.uncertain {
            return Err(failure(
                "outcome_unknown",
                "This page's outcome is unknown; take a fresh snapshot first",
            ));
        }
        let screenshot = match tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(failure("cancelled", "Browser screenshot cancelled")),
            result = self.provider.screenshot(&session, &page.key, cancel) => result,
        } {
            Ok(screenshot) => screenshot,
            Err(error) => {
                if error.uncertain {
                    state.fail_unknown();
                }
                return Err(error.into_boundary());
            }
        };
        drop(state);
        if let Err(error) = provider_field(
            &screenshot.mime,
            MAX_PROVIDER_MIME_BYTES,
            "Browser provider returned an invalid screenshot type",
        )
        .and_then(|()| {
            provider_field(
                &screenshot.filename,
                MAX_PROVIDER_FILENAME_BYTES,
                "Browser provider returned an invalid screenshot filename",
            )
        }) {
            return Err(error.into_boundary());
        }
        Ok(ScreenshotView {
            sha256: digest_hex(&screenshot.bytes),
            bytes: screenshot.bytes.len(),
            raw_bytes: screenshot.bytes,
            mime: screenshot.mime,
            filename: screenshot.filename,
            width: screenshot.width,
            height: screenshot.height,
            media_ref: None,
            model_image_attached: false,
        })
    }

    pub async fn download(
        &self,
        context: &ToolContext,
        page: &str,
        cancel: &dyn Cancellation,
    ) -> Result<DownloadView, BoundaryError> {
        let (session, page) = self.active_page(context, page).await?;
        let mut state = page.document.lock().await;
        if state.uncertain {
            return Err(failure(
                "outcome_unknown",
                "This page's outcome is unknown; take a fresh snapshot first",
            ));
        }
        if cancel.is_cancelled() {
            return Err(failure("cancelled", "Browser download cancelled"));
        }
        let download = match tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                state.fail_unknown();
                return Err(BrowserFailure::uncertain(
                    "Browser download was cancelled after dispatch",
                )
                .into_boundary());
            }
            result = self.provider.download(&session, &page.key, cancel) => result,
        } {
            Ok(download) => download,
            Err(error) => {
                if error.uncertain {
                    state.fail_unknown();
                }
                return Err(error.into_boundary());
            }
        };
        drop(state);
        if let Err(error) = provider_field(
            &download.filename,
            MAX_PROVIDER_FILENAME_BYTES,
            "Browser provider returned an invalid download filename",
        )
        .and_then(|()| {
            provider_field(
                &download.mime,
                MAX_PROVIDER_MIME_BYTES,
                "Browser provider returned an invalid download type",
            )
        }) {
            return Err(error.into_boundary());
        }
        Ok(DownloadView {
            sha256: digest_hex(&download.bytes),
            bytes: download.bytes.len(),
            filename: download.filename,
            mime: download.mime,
            artifact_stored: false,
        })
    }

    pub async fn upload(
        &self,
        context: &ToolContext,
        page: &str,
        document: &str,
        element: &str,
        artifact: &str,
        cancel: &dyn Cancellation,
    ) -> Result<UploadView, BoundaryError> {
        let (session, page_state) = self.active_page(context, page).await?;
        let mut state = page_state.document.lock().await;
        let provider_element = Self::verify_document(&state, document, element)?;
        if cancel.is_cancelled() {
            return Err(failure("cancelled", "Browser upload cancelled"));
        }
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                state.fail_unknown();
                return Err(BrowserFailure::uncertain(
                    "Browser upload was cancelled after dispatch",
                )
                .into_boundary());
            }
            result = self
                .provider
                .upload(&session, &page_state.key, &provider_element, artifact, cancel) => result,
        };
        state.invalidate();
        let revision = state.revision;
        drop(state);
        match result {
            Ok(upload) => {
                if let Err(error) = provider_field(
                    &upload.filename,
                    MAX_PROVIDER_FILENAME_BYTES,
                    "Browser provider returned an invalid upload filename",
                ) {
                    page_state.document.lock().await.fail_unknown();
                    return Err(error.into_boundary());
                }
                Ok(UploadView {
                    filename: upload.filename,
                    bytes_sent: upload.bytes_sent,
                    document_revision: revision,
                })
            }
            Err(error) => {
                if error.uncertain {
                    page_state.document.lock().await.fail_unknown();
                }
                Err(error.into_boundary())
            }
        }
    }

    /// Record that host cleanup could not confirm the provider state. Only a
    /// complete close or shutdown may clear it.
    pub(crate) async fn mark_shutdown_unknown(&self) {
        self.inner.lock().await.open_uncertain = true;
    }

    /// Release the provider and private session. Repeated calls are safe. A
    /// failed close or shutdown keeps the state required for a retry and never
    /// reports success before the provider confirms it.
    pub async fn shutdown(&self, cancel: &dyn Cancellation) -> Result<(), BoundaryError> {
        let _serial = self.shutdown_lock.lock().await;
        let (session, pages, provider_shutdown, open_uncertain) = {
            let inner = self.inner.lock().await;
            (
                inner.session.clone(),
                inner.pages.values().cloned().collect::<Vec<_>>(),
                inner.provider_shutdown,
                inner.open_uncertain,
            )
        };
        if provider_shutdown && !open_uncertain && session.is_none() && pages.is_empty() {
            self.inner.lock().await.closed = true;
            return Ok(());
        }
        let mut guards = Vec::with_capacity(pages.len());
        for page in &pages {
            guards.push(page.document.lock().await);
        }
        if let Some(session) = session.as_deref() {
            match self.provider.close(session, cancel).await {
                Ok(()) => {
                    let mut inner = self.inner.lock().await;
                    if inner.session.as_deref() == Some(session) {
                        inner.session = None;
                        inner.pages.clear();
                    }
                    inner.closed = true;
                }
                Err(error) => {
                    if error.uncertain {
                        self.inner.lock().await.open_uncertain = true;
                    }
                    return Err(error.into_boundary());
                }
            }
        }
        match self.provider.shutdown(cancel).await {
            Ok(()) => {
                let mut inner = self.inner.lock().await;
                inner.provider_shutdown = true;
                inner.open_uncertain = false;
                inner.session = None;
                inner.pages.clear();
                inner.closed = true;
                Ok(())
            }
            Err(error) => {
                // The provider may still hold resources. Keep the retryable
                // state and conservatively block open and page replay.
                self.inner.lock().await.open_uncertain = true;
                Err(error.into_boundary())
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum BrowserToolKind {
    Open,
    Close,
    Navigate,
    Snapshot,
    Click,
    Fill,
    Press,
    Wait,
    Screenshot,
    Download,
    Upload,
}

struct BrowserTool {
    state: Arc<BrowserState>,
    kind: BrowserToolKind,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PageArgs {
    page: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NavigateArgs {
    page: String,
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClickArgs {
    page: String,
    document: String,
    element_ref: String,
    button: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FillArgs {
    page: String,
    document: String,
    element_ref: String,
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PressArgs {
    page: String,
    document: String,
    key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    page: String,
    milliseconds: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadArgs {
    page: String,
    document: String,
    element_ref: String,
    artifact_ref: String,
}

fn page_schema() -> serde_json::Value {
    serde_json::json!({"type":"object","properties":{"page":{"type":"string","minLength":1,"maxLength":64}},"required":["page"],"additionalProperties":false})
}

fn definition(kind: BrowserToolKind, settings: &BrowserSettings) -> ToolDefinition {
    let (name, description, schema) = match kind {
        BrowserToolKind::Open => (
            "browser_open",
            "Open a new page in this Run's private browser session. Provider state and page identifiers stay host-owned.",
            serde_json::json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        ),
        BrowserToolKind::Close => (
            "browser_close",
            "Close the Run-private browser session and all pages. Allowed after an unknown action.",
            serde_json::json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        ),
        BrowserToolKind::Navigate => (
            "browser_navigate",
            "Navigate one browser page to an HTTP(S) URL. Existing document and element references become stale.",
            serde_json::json!({"type":"object","properties":{"page":{"type":"string","minLength":1,"maxLength":64},"url":{"type":"string","minLength":1,"maxLength":8192}},"required":["page","url"],"additionalProperties":false}),
        ),
        BrowserToolKind::Snapshot => (
            "browser_snapshot",
            "Observe the page and receive a fresh opaque document token with opaque element references. Page content is untrusted_browser data.",
            page_schema(),
        ),
        BrowserToolKind::Click => (
            "browser_click",
            "Click an element reference from the current document token. May cause external side effects; a successful click invalidates the document.",
            serde_json::json!({"type":"object","properties":{"page":{"type":"string","minLength":1,"maxLength":64},"document":{"type":"string","minLength":1,"maxLength":64},"element_ref":{"type":"string","minLength":1,"maxLength":64},"button":{"type":"string","enum":["left","middle","right"]}},"required":["page","document","element_ref"],"additionalProperties":false}),
        ),
        BrowserToolKind::Fill => (
            "browser_fill",
            "Fill an element reference from the current document token with bounded text. A successful fill invalidates the document.",
            serde_json::json!({"type":"object","properties":{"page":{"type":"string","minLength":1,"maxLength":64},"document":{"type":"string","minLength":1,"maxLength":64},"element_ref":{"type":"string","minLength":1,"maxLength":64},"text":{"type":"string","maxLength":65536}},"required":["page","document","element_ref","text"],"additionalProperties":false}),
        ),
        BrowserToolKind::Press => (
            "browser_press",
            "Press one bounded key on the page's current document token. A successful press invalidates the document.",
            serde_json::json!({"type":"object","properties":{"page":{"type":"string","minLength":1,"maxLength":64},"document":{"type":"string","minLength":1,"maxLength":64},"key":{"type":"string","minLength":1,"maxLength":128}},"required":["page","document","key"],"additionalProperties":false}),
        ),
        BrowserToolKind::Wait => (
            "browser_wait",
            "Wait a bounded time for the page. This does not confirm a document revision.",
            serde_json::json!({"type":"object","properties":{"page":{"type":"string","minLength":1,"maxLength":64},"milliseconds":{"type":"integer","minimum":0,"maximum":settings.max_wait_ms}},"required":["page","milliseconds"],"additionalProperties":false}),
        ),
        BrowserToolKind::Screenshot => (
            "browser_screenshot",
            "Capture a bounded screenshot through the host provider. This build returns verified metadata and does not attach model images.",
            page_schema(),
        ),
        BrowserToolKind::Download => (
            "browser_download",
            "Request the page's pending download through the host provider. This build returns verified metadata only.",
            page_schema(),
        ),
        BrowserToolKind::Upload => (
            "browser_upload",
            "Upload an authorized artifact reference to an element reference in the current document token. This build verifies metadata through the provider.",
            serde_json::json!({"type":"object","properties":{"page":{"type":"string","minLength":1,"maxLength":64},"document":{"type":"string","minLength":1,"maxLength":64},"element_ref":{"type":"string","minLength":1,"maxLength":64},"artifact_ref":{"type":"string","minLength":1,"maxLength":256}},"required":["page","document","element_ref","artifact_ref"],"additionalProperties":false}),
        ),
    };
    ToolDefinition {
        name: name.into(),
        description: description.into(),
        input_schema: schema,
    }
}

fn parse_page(call: &ToolCall) -> Result<String, BoundaryError> {
    if call.arguments.len() > 64 * 1024 {
        return Err(failure(
            "invalid_args",
            "Browser arguments exceed their limit",
        ));
    }
    let args: PageArgs = serde_json::from_str(&call.arguments)
        .map_err(|_| failure("invalid_args", "invalid browser page arguments"))?;
    if !valid_token(&args.page, "page_") {
        return Err(failure("invalid_args", "invalid browser page identity"));
    }
    Ok(args.page)
}

fn capture_snapshot(
    store: Option<&OutputStore>,
    call: &ToolCall,
    context: &ToolContext,
    limit: usize,
    text: &str,
    source_truncated: bool,
) -> (Vec<SealedOutput>, Vec<ErrorInfo>) {
    let Some(store) = store else {
        return (Vec::new(), Vec::new());
    };
    let scope = OutputScope {
        session_id: context.run.session_id,
        run_id: context.run.id,
        call_id: call.call_id.clone(),
        stream: OutputStream::Browser,
    };
    let mut capture = match store.begin(scope, limit) {
        Ok(capture) => capture,
        Err(error) => return (Vec::new(), vec![error.0]),
    };
    if let Err(error) = capture.push(text.as_bytes()) {
        return (Vec::new(), vec![error.0]);
    }
    let capture_incomplete =
        capture.capture_limit_bytes() == 0 || text.len() > capture.capture_limit_bytes();
    let completion = if source_truncated || capture_incomplete {
        CaptureCompletion::LimitReached
    } else {
        CaptureCompletion::Eof
    };
    match capture.seal(completion) {
        Ok(sealed) => (vec![sealed], Vec::new()),
        Err(error) => (Vec::new(), vec![error.0]),
    }
}

impl BrowserTool {
    async fn execute_captured(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        cancel: &dyn Cancellation,
        outputs: Option<&OutputStore>,
    ) -> Result<CapturedToolResult, BoundaryError> {
        if cancel.is_cancelled() {
            return Err(failure("cancelled", "Browser action cancelled"));
        }
        let value = match self.kind {
            BrowserToolKind::Open => {
                let view = self.state.open(context, cancel).await?;
                serde_json::json!({"source":UNTRUSTED_BROWSER,"page":view.page,"url":view.url})
            }
            BrowserToolKind::Close => {
                self.state.close(context, cancel).await?;
                serde_json::json!({"source":UNTRUSTED_BROWSER,"closed":true})
            }
            BrowserToolKind::Navigate => {
                let args: NavigateArgs = serde_json::from_str(&call.arguments)
                    .map_err(|_| failure("invalid_args", "invalid browser navigate arguments"))?;
                if !valid_token(&args.page, "page_") {
                    return Err(failure("invalid_args", "invalid browser page identity"));
                }
                let view = self
                    .state
                    .navigate(context, &args.page, &args.url, cancel)
                    .await?;
                serde_json::json!({"source":UNTRUSTED_BROWSER,"page":view.page,"url":view.url,"document_stale":view.document_stale})
            }
            BrowserToolKind::Snapshot => {
                let page = parse_page(call)?;
                let view = self.state.snapshot(context, &page, cancel).await?;
                // Capture the complete host projection so a long snapshot
                // retains page/document/elements/url instead of only its body.
                let projection = serde_json::json!({
                    "source": UNTRUSTED_BROWSER,
                    "page": view.page,
                    "document": view.document,
                    "document_revision": view.document_revision,
                    "url": view.url,
                    "title": view.title,
                    "text": view.text,
                    "elements": view.elements,
                    "truncated": view.truncated,
                })
                .to_string();
                let (sealed, capture_errors) = capture_snapshot(
                    outputs,
                    call,
                    context,
                    self.state.settings.max_snapshot_bytes,
                    &projection,
                    view.truncated,
                );
                let metadata: Vec<OutputMetadata> = sealed
                    .iter()
                    .map(|output| output.metadata.clone())
                    .collect();
                let capture_lost = metadata
                    .iter()
                    .any(|item| item.retained_bytes == 0 || item.withheld_reason.is_some());
                let mut data = serde_json::json!({
                    "source":UNTRUSTED_BROWSER,
                    "page":view.page,
                    "document":view.document,
                    "document_revision":view.document_revision,
                    "url":view.url,
                    "title":view.title,
                    "elements":view.elements,
                    "truncated":view.truncated,
                });
                if !metadata.is_empty() {
                    data["output_refs"] = serde_json::to_value(&metadata).map_err(|_| {
                        failure("serialization_error", "Browser snapshot cannot be encoded")
                    })?;
                    data["preview"] = serde_json::Value::String(
                        metadata
                            .first()
                            .map(|item| item.preview_head.clone())
                            .unwrap_or_default(),
                    );
                }
                let mut inline_truncated = false;
                if sealed.is_empty() || capture_lost {
                    // No usable capture (disabled store, withheld output or a
                    // zero-byte retention) still returns a bounded inline
                    // observation instead of an empty success.
                    let (text, text_truncated) = bounded_text(&view.text, INLINE_SNAPSHOT_BYTES);
                    inline_truncated = text_truncated;
                    data["text"] = serde_json::Value::String(text);
                }
                let captured_truncated = metadata.iter().any(|item| !item.artifact_complete)
                    || (capture_lost && !metadata.is_empty());
                let truncated = view.truncated || inline_truncated || captured_truncated;
                data["truncated"] = serde_json::Value::Bool(truncated);
                return Ok(CapturedToolResult {
                    result: ToolResult {
                        ok: true,
                        data: Some(data),
                        error: None,
                        truncated,
                    },
                    outputs: sealed,
                    capture_errors,
                });
            }
            BrowserToolKind::Click => {
                let args: ClickArgs = serde_json::from_str(&call.arguments)
                    .map_err(|_| failure("invalid_args", "invalid browser click arguments"))?;
                if !valid_token(&args.page, "page_")
                    || !valid_token(&args.document, "doc_")
                    || !valid_token(&args.element_ref, "el_")
                {
                    return Err(failure("invalid_args", "invalid browser reference"));
                }
                let view = self
                    .state
                    .click(
                        context,
                        &args.page,
                        &args.document,
                        &args.element_ref,
                        args.button.as_deref(),
                        cancel,
                    )
                    .await?;
                serde_json::json!({"source":UNTRUSTED_BROWSER,"page":view.page,"url":view.url,"detail":view.detail,"document_stale":view.document_stale,"document_revision":view.document_revision})
            }
            BrowserToolKind::Fill => {
                let args: FillArgs = serde_json::from_str(&call.arguments)
                    .map_err(|_| failure("invalid_args", "invalid browser fill arguments"))?;
                if !valid_token(&args.page, "page_")
                    || !valid_token(&args.document, "doc_")
                    || !valid_token(&args.element_ref, "el_")
                    || args.text.len() > MAX_FILL_BYTES
                    || args.text.contains('\0')
                {
                    return Err(failure("invalid_args", "invalid browser fill arguments"));
                }
                let view = self
                    .state
                    .fill(
                        context,
                        &args.page,
                        &args.document,
                        &args.element_ref,
                        &args.text,
                        cancel,
                    )
                    .await?;
                serde_json::json!({"source":UNTRUSTED_BROWSER,"page":view.page,"url":view.url,"detail":view.detail,"document_stale":view.document_stale,"document_revision":view.document_revision})
            }
            BrowserToolKind::Press => {
                let args: PressArgs = serde_json::from_str(&call.arguments)
                    .map_err(|_| failure("invalid_args", "invalid browser press arguments"))?;
                if !valid_token(&args.page, "page_")
                    || !valid_token(&args.document, "doc_")
                    || args.key.is_empty()
                    || args.key.len() > 128
                    || args.key.chars().any(char::is_control)
                {
                    return Err(failure("invalid_args", "invalid browser press arguments"));
                }
                let view = self
                    .state
                    .press(context, &args.page, &args.document, &args.key, cancel)
                    .await?;
                serde_json::json!({"source":UNTRUSTED_BROWSER,"page":view.page,"url":view.url,"detail":view.detail,"document_stale":view.document_stale,"document_revision":view.document_revision})
            }
            BrowserToolKind::Wait => {
                let args: WaitArgs = serde_json::from_str(&call.arguments)
                    .map_err(|_| failure("invalid_args", "invalid browser wait arguments"))?;
                if !valid_token(&args.page, "page_") {
                    return Err(failure("invalid_args", "invalid browser page identity"));
                }
                let view = self
                    .state
                    .wait(context, &args.page, args.milliseconds, cancel)
                    .await?;
                serde_json::json!({"source":UNTRUSTED_BROWSER,"page":view.page,"url":view.url,"detail":view.detail,"document_stale":view.document_stale,"document_revision":view.document_revision})
            }
            BrowserToolKind::Screenshot => {
                let page = parse_page(call)?;
                let view = self.state.screenshot(context, &page, cancel).await?;
                let encoded = serde_json::to_value(&view).map_err(|_| {
                    failure(
                        "serialization_error",
                        "Browser screenshot cannot be encoded",
                    )
                })?;
                // DEV-079 returns verified metadata only. Screenshot media is
                // captured and injected by DEV-082 once pinned storage exists.
                serde_json::json!({
                    "source":UNTRUSTED_BROWSER,
                    "screenshot":encoded,
                })
            }
            BrowserToolKind::Download => {
                let page = parse_page(call)?;
                let view = self.state.download(context, &page, cancel).await?;
                serde_json::json!({
                    "source":UNTRUSTED_BROWSER,
                    "download":serde_json::to_value(view).map_err(|_| {
                        failure("serialization_error", "Browser download cannot be encoded")
                    })?,
                })
            }
            BrowserToolKind::Upload => {
                let args: UploadArgs = serde_json::from_str(&call.arguments)
                    .map_err(|_| failure("invalid_args", "invalid browser upload arguments"))?;
                if !valid_token(&args.page, "page_")
                    || !valid_token(&args.document, "doc_")
                    || !valid_token(&args.element_ref, "el_")
                    || args.artifact_ref.is_empty()
                    || args.artifact_ref.len() > 256
                    || args.artifact_ref.chars().any(char::is_control)
                {
                    return Err(failure("invalid_args", "invalid browser upload arguments"));
                }
                let view = self
                    .state
                    .upload(
                        context,
                        &args.page,
                        &args.document,
                        &args.element_ref,
                        &args.artifact_ref,
                        cancel,
                    )
                    .await?;
                serde_json::json!({
                    "source":UNTRUSTED_BROWSER,
                    "upload":serde_json::to_value(view).map_err(|_| {
                        failure("serialization_error", "Browser upload cannot be encoded")
                    })?,
                })
            }
        };
        Ok(CapturedToolResult {
            result: ToolResult {
                ok: true,
                data: Some(value),
                error: None,
                truncated: false,
            },
            outputs: Vec::new(),
            capture_errors: Vec::new(),
        })
    }
}

impl Tool for BrowserTool {
    fn definition(&self) -> ToolDefinition {
        definition(self.kind, &self.state.settings)
    }

    fn execute_captured<'a>(
        &'a self,
        call: &'a ToolCall,
        context: &'a ToolContext,
        cancel: &'a dyn Cancellation,
        outputs: Option<&'a OutputStore>,
    ) -> Operation<'a, CapturedToolResult> {
        Box::pin(async move { self.execute_captured(call, context, cancel, outputs).await })
    }

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        context: &'a ToolContext,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, ToolResult> {
        Box::pin(async move {
            self.execute_captured(call, context, cancel, None)
                .await
                .map(|capture| capture.result)
        })
    }
}

/// Register the exact browser tool surface supported by the provider facts.
pub(crate) fn register_tools(
    state: &Arc<BrowserState>,
    dispatcher: &ToolDispatcher,
) -> Result<(), BoundaryError> {
    let facts = state.facts.clone();
    let kinds = [
        BrowserToolKind::Open,
        BrowserToolKind::Close,
        BrowserToolKind::Navigate,
        BrowserToolKind::Snapshot,
        BrowserToolKind::Click,
        BrowserToolKind::Fill,
        BrowserToolKind::Press,
        BrowserToolKind::Wait,
    ];
    for kind in kinds {
        let tool = BrowserTool {
            state: state.clone(),
            kind,
        };
        dispatcher.register_builtin_adapter(tool.definition(), Arc::new(tool))?;
    }
    for (enabled, kind) in [
        (facts.screenshots, BrowserToolKind::Screenshot),
        (facts.downloads, BrowserToolKind::Download),
        (facts.uploads, BrowserToolKind::Upload),
    ] {
        if enabled {
            let tool = BrowserTool {
                state: state.clone(),
                kind,
            };
            dispatcher.register_builtin_adapter(tool.definition(), Arc::new(tool))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Controllable offline provider
// ---------------------------------------------------------------------------

/// Deterministic provider for state and permission tests. It performs no
/// network access and exposes bounded, inspectable call counters.
pub struct FixtureProvider {
    facts: BrowserFacts,
    delay_ms: u64,
    session: Mutex<Option<String>>,
    pages: Mutex<BTreeMap<String, String>>,
    active: AtomicU64,
    max_active: AtomicU64,
    snapshots: AtomicU64,
    shutdowns: AtomicU64,
    fail_unknown: Mutex<bool>,
    fail_close_known: Mutex<bool>,
    fail_open_known: Mutex<bool>,
    fail_shutdown_unknown: Mutex<bool>,
    stall_open: AtomicBool,
    stall_action: AtomicBool,
    snapshot_truncated: AtomicBool,
    override_action_url: Mutex<Option<String>>,
    override_title: Mutex<Option<String>>,
    shutdown_delay_ms: u64,
    next_element: AtomicU64,
}
impl FixtureProvider {
    pub fn new(facts: BrowserFacts) -> Self {
        Self {
            facts,
            delay_ms: 0,
            session: Mutex::new(None),
            pages: Mutex::new(BTreeMap::new()),
            active: AtomicU64::new(0),
            max_active: AtomicU64::new(0),
            snapshots: AtomicU64::new(0),
            shutdowns: AtomicU64::new(0),
            fail_unknown: Mutex::new(false),
            fail_close_known: Mutex::new(false),
            fail_open_known: Mutex::new(false),
            fail_shutdown_unknown: Mutex::new(false),
            stall_open: AtomicBool::new(false),
            stall_action: AtomicBool::new(false),
            snapshot_truncated: AtomicBool::new(false),
            override_action_url: Mutex::new(None),
            override_title: Mutex::new(None),
            shutdown_delay_ms: 0,
            next_element: AtomicU64::new(1),
        }
    }
    pub fn full() -> Self {
        Self::new(BrowserFacts {
            provider: "fixture".into(),
            screenshots: true,
            downloads: true,
            uploads: true,
        })
    }
    pub fn with_delay(mut self, delay_ms: u64) -> Self {
        self.delay_ms = delay_ms;
        self
    }
    pub fn with_shutdown_delay(mut self, delay_ms: u64) -> Self {
        self.shutdown_delay_ms = delay_ms;
        self
    }
    pub fn fail_next_unknown(&self) {
        if let Ok(mut value) = self.fail_unknown.lock() {
            *value = true;
        }
    }
    pub fn fail_next_close_known(&self) {
        if let Ok(mut value) = self.fail_close_known.lock() {
            *value = true;
        }
    }
    pub fn fail_next_open_known(&self) {
        if let Ok(mut value) = self.fail_open_known.lock() {
            *value = true;
        }
    }
    pub fn fail_next_shutdown_unknown(&self) {
        if let Ok(mut value) = self.fail_shutdown_unknown.lock() {
            *value = true;
        }
    }
    /// Make the next open wait until cancelled, so tests observe the
    /// post-dispatch cancellation branch deterministically.
    pub fn stall_next_open(&self) {
        self.stall_open.store(true, Ordering::Release);
    }
    /// Make the next dispatched page action wait until cancelled.
    pub fn stall_next_action(&self) {
        self.stall_action.store(true, Ordering::Release);
    }
    /// Report provider-side truncation once from the next snapshot.
    pub fn truncate_next_snapshot(&self) {
        self.snapshot_truncated.store(true, Ordering::Release);
    }
    /// Make the next navigation report this provider-side final URL.
    pub fn return_next_action_url(&self, url: impl Into<String>) {
        if let Ok(mut value) = self.override_action_url.lock() {
            *value = Some(url.into());
        }
    }
    /// Make the next snapshot report this provider-side title.
    pub fn return_next_snapshot_title(&self, title: impl Into<String>) {
        if let Ok(mut value) = self.override_title.lock() {
            *value = Some(title.into());
        }
    }
    fn take_override_action_url(&self) -> Option<String> {
        self.override_action_url
            .lock()
            .ok()
            .and_then(|mut value| value.take())
    }
    fn take_override_title(&self) -> Option<String> {
        self.override_title
            .lock()
            .ok()
            .and_then(|mut value| value.take())
    }
    fn take_stall_open(&self) -> bool {
        self.stall_open.swap(false, Ordering::AcqRel)
    }
    fn take_stall_action(&self) -> bool {
        self.stall_action.swap(false, Ordering::AcqRel)
    }
    fn take_snapshot_truncated(&self) -> bool {
        self.snapshot_truncated.swap(false, Ordering::AcqRel)
    }
    pub fn snapshot_count(&self) -> u64 {
        self.snapshots.load(Ordering::Acquire)
    }
    pub fn shutdown_count(&self) -> u64 {
        self.shutdowns.load(Ordering::Acquire)
    }
    pub fn max_concurrency(&self) -> u64 {
        self.max_active.load(Ordering::Acquire)
    }
    fn enter(&self) {
        let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.max_active.fetch_max(active, Ordering::AcqRel);
    }
    fn leave(&self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
    fn take_unknown(&self) -> bool {
        self.fail_unknown
            .lock()
            .map(|mut value| std::mem::replace(&mut *value, false))
            .unwrap_or(false)
    }
    fn take_close_known(&self) -> bool {
        self.fail_close_known
            .lock()
            .map(|mut value| std::mem::replace(&mut *value, false))
            .unwrap_or(false)
    }
    fn take_open_known(&self) -> bool {
        self.fail_open_known
            .lock()
            .map(|mut value| std::mem::replace(&mut *value, false))
            .unwrap_or(false)
    }
    fn take_shutdown_unknown(&self) -> bool {
        self.fail_shutdown_unknown
            .lock()
            .map(|mut value| std::mem::replace(&mut *value, false))
            .unwrap_or(false)
    }
    fn cancelled(cancel: &dyn Cancellation) -> Result<(), BrowserFailure> {
        if cancel.is_cancelled() {
            Err(BrowserFailure::new(
                "cancelled",
                "fixture provider observed cancellation",
            ))
        } else {
            Ok(())
        }
    }
    async fn pause(
        &self,
        cancel: &dyn Cancellation,
        milliseconds: u64,
    ) -> Result<(), BrowserFailure> {
        if milliseconds == 0 {
            return Self::cancelled(cancel);
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(BrowserFailure::new(
                "cancelled",
                "fixture provider observed cancellation",
            )),
            _ = tokio::time::sleep(std::time::Duration::from_millis(milliseconds)) => Ok(()),
        }
    }
}

#[async_trait::async_trait]
impl BrowserProvider for FixtureProvider {
    fn facts(&self) -> BrowserFacts {
        self.facts.clone()
    }
    async fn open(
        &self,
        session: Option<&str>,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserOpened, BrowserFailure> {
        Self::cancelled(cancel)?;
        if self.take_stall_open() {
            std::future::pending::<()>().await;
        }
        if self.take_open_known() {
            return Err(BrowserFailure::new(
                "open_failed",
                "fixture open failed without side effects",
            ));
        }
        if self.take_unknown() {
            return Err(BrowserFailure::uncertain(
                "fixture open may have created provider resources",
            ));
        }
        let session = match session {
            Some(session) => session.to_owned(),
            None => opaque("session"),
        };
        *self
            .session
            .lock()
            .map_err(|_| BrowserFailure::new("provider_state", "fixture session lock failed"))? =
            Some(session.clone());
        let page = opaque("provider_page");
        self.pages
            .lock()
            .map_err(|_| BrowserFailure::new("provider_state", "fixture page lock failed"))?
            .insert(page.clone(), "about:blank".into());
        Ok(BrowserOpened {
            session,
            page,
            url: Some("about:blank".into()),
        })
    }
    async fn close(&self, _session: &str, cancel: &dyn Cancellation) -> Result<(), BrowserFailure> {
        Self::cancelled(cancel)?;
        if self.take_close_known() {
            return Err(BrowserFailure::new(
                "close_failed",
                "fixture close failed without side effects",
            ));
        }
        self.pages
            .lock()
            .map_err(|_| BrowserFailure::new("provider_state", "fixture page lock failed"))?
            .clear();
        Ok(())
    }
    async fn navigate(
        &self,
        _session: &str,
        page: &str,
        url: &str,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserAction, BrowserFailure> {
        if self.take_stall_action() {
            std::future::pending::<()>().await;
        }
        self.pause(cancel, self.delay_ms).await?;
        self.pages
            .lock()
            .map_err(|_| BrowserFailure::new("provider_state", "fixture page lock failed"))?
            .insert(page.into(), url.into());
        let final_url = self
            .take_override_action_url()
            .unwrap_or_else(|| url.to_owned());
        Ok(BrowserAction {
            url: Some(final_url),
            detail: Some("navigated".into()),
        })
    }
    async fn snapshot(
        &self,
        _session: &str,
        page: &str,
        max_bytes: usize,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserSnapshot, BrowserFailure> {
        self.enter();
        let result = async {
            self.pause(cancel, self.delay_ms).await?;
            let url = self
                .pages
                .lock()
                .map_err(|_| BrowserFailure::new("provider_state", "fixture page lock failed"))?
                .get(page)
                .cloned()
                .ok_or_else(|| BrowserFailure::new("invalid_page", "fixture page is unknown"))?;
            let key = format!(
                "fixture-element-{}",
                self.next_element.fetch_add(1, Ordering::AcqRel)
            );
            let text = "Fixture page content".to_owned();
            let _ = max_bytes;
            Ok(BrowserSnapshot {
                url,
                title: Some(
                    self.take_override_title()
                        .unwrap_or_else(|| "Fixture".into()),
                ),
                text,
                elements: vec![BrowserElement {
                    key,
                    role: "button".into(),
                    name: "Submit".into(),
                }],
                truncated: self.take_snapshot_truncated(),
            })
        }
        .await;
        self.leave();
        if result.is_ok() {
            self.snapshots.fetch_add(1, Ordering::AcqRel);
        }
        result
    }
    async fn click(
        &self,
        _session: &str,
        _page: &str,
        _element: &str,
        button: Option<&str>,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserAction, BrowserFailure> {
        Self::cancelled(cancel)?;
        if self.take_stall_action() {
            std::future::pending::<()>().await;
        }
        if self.take_unknown() {
            return Err(BrowserFailure::uncertain(
                "fixture click may have been dispatched",
            ));
        }
        Ok(BrowserAction {
            url: None,
            detail: Some(format!("clicked {}", button.unwrap_or("left"))),
        })
    }
    async fn fill(
        &self,
        _session: &str,
        _page: &str,
        _element: &str,
        text: &str,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserAction, BrowserFailure> {
        Self::cancelled(cancel)?;
        if self.take_stall_action() {
            std::future::pending::<()>().await;
        }
        Ok(BrowserAction {
            url: None,
            detail: Some(format!("filled {} bytes", text.len())),
        })
    }
    async fn press(
        &self,
        _session: &str,
        _page: &str,
        key: &str,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserAction, BrowserFailure> {
        Self::cancelled(cancel)?;
        if self.take_stall_action() {
            std::future::pending::<()>().await;
        }
        Ok(BrowserAction {
            url: None,
            detail: Some(format!("pressed {key}")),
        })
    }
    async fn wait(
        &self,
        _session: &str,
        _page: &str,
        milliseconds: u64,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserAction, BrowserFailure> {
        self.pause(cancel, milliseconds).await?;
        Ok(BrowserAction {
            url: self.take_override_action_url(),
            detail: Some(format!("waited {milliseconds}ms")),
        })
    }
    async fn screenshot(
        &self,
        _session: &str,
        _page: &str,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserScreenshot, BrowserFailure> {
        Self::cancelled(cancel)?;
        Ok(BrowserScreenshot {
            mime: "image/png".into(),
            filename: "screenshot.png".into(),
            bytes: b"\x89PNG\r\n\x1a\nfixture".to_vec(),
            width: 640,
            height: 480,
        })
    }
    async fn download(
        &self,
        _session: &str,
        _page: &str,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserDownload, BrowserFailure> {
        Self::cancelled(cancel)?;
        if self.take_stall_action() {
            std::future::pending::<()>().await;
        }
        Ok(BrowserDownload {
            filename: "download.txt".into(),
            mime: "text/plain".into(),
            bytes: b"fixture download".to_vec(),
        })
    }
    async fn upload(
        &self,
        _session: &str,
        _page: &str,
        _element: &str,
        artifact: &str,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserUpload, BrowserFailure> {
        Self::cancelled(cancel)?;
        if self.take_stall_action() {
            std::future::pending::<()>().await;
        }
        Ok(BrowserUpload {
            filename: artifact.into(),
            bytes_sent: 7,
        })
    }
    async fn shutdown(&self, cancel: &dyn Cancellation) -> Result<(), BrowserFailure> {
        Self::cancelled(cancel)?;
        self.pause(cancel, self.shutdown_delay_ms).await?;
        if self.take_shutdown_unknown() {
            return Err(BrowserFailure::uncertain(
                "fixture shutdown outcome is unknown",
            ));
        }
        self.shutdowns.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{PermissionMode, RunId, WorkMode};
    use std::sync::atomic::AtomicBool;

    struct NeverCancel;
    impl Cancellation for NeverCancel {
        fn is_cancelled(&self) -> bool {
            false
        }
        fn cancelled(&self) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            Box::pin(std::future::pending())
        }
    }

    struct SwitchCancel(Arc<AtomicBool>);
    impl Cancellation for SwitchCancel {
        fn is_cancelled(&self) -> bool {
            self.0.load(Ordering::Acquire)
        }
        fn cancelled(&self) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            let flag = self.0.clone();
            Box::pin(async move {
                while !flag.load(Ordering::Acquire) {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
            })
        }
    }

    fn make_context(run: Run) -> ToolContext {
        ToolContext {
            run,
            workspace: std::path::PathBuf::from("."),
        }
    }
    fn make_run() -> Run {
        Run {
            id: RunId::new(),
            session_id: crate::contract::SessionId::new(),
            work_mode: WorkMode::Work,
            permissions: PermissionMode::Auto,
        }
    }
    fn make_state(provider: Arc<FixtureProvider>, run: &Run) -> BrowserState {
        BrowserState::new(
            BrowserSettings {
                enabled: true,
                ..Default::default()
            },
            provider,
            run.clone(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn snapshot_replaces_document_and_old_elements_become_stale() {
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = make_state(provider.clone(), &run);
        let context = make_context(run);
        let cancel = NeverCancel;
        let opened = state.open(&context, &cancel).await.unwrap();
        let first = state
            .snapshot(&context, &opened.page, &cancel)
            .await
            .unwrap();
        assert_eq!(first.elements.len(), 1);
        assert_eq!(first.elements[0].name, "Submit");
        let second = state
            .snapshot(&context, &opened.page, &cancel)
            .await
            .unwrap();
        assert_ne!(first.document, second.document);
        assert!(second.document_revision > first.document_revision);
        assert_eq!(
            state
                .click(
                    &context,
                    &opened.page,
                    &first.document,
                    &first.elements[0].element_ref,
                    None,
                    &cancel,
                )
                .await
                .unwrap_err()
                .0
                .code,
            "stale_element"
        );
        assert_eq!(provider.snapshot_count(), 2);
    }

    #[tokio::test]
    async fn same_page_actions_are_serialized() {
        let provider = Arc::new(FixtureProvider::full().with_delay(30));
        let run = make_run();
        let state = Arc::new(make_state(provider.clone(), &run));
        let context = make_context(run);
        let cancel = NeverCancel;
        let opened = state.open(&context, &cancel).await.unwrap();
        let (left, right) = tokio::join!(
            state.snapshot(&context, &opened.page, &cancel),
            state.snapshot(&context, &opened.page, &cancel)
        );
        left.unwrap();
        right.unwrap();
        assert_eq!(provider.max_concurrency(), 1);
    }

    #[tokio::test]
    async fn unknown_action_is_bound_to_one_page_only() {
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = make_state(provider.clone(), &run);
        let context = make_context(run);
        let cancel = NeverCancel;
        let first = state.open(&context, &cancel).await.unwrap();
        let second = state.open(&context, &cancel).await.unwrap();
        let first_snapshot = state
            .snapshot(&context, &first.page, &cancel)
            .await
            .unwrap();
        state
            .snapshot(&context, &second.page, &cancel)
            .await
            .unwrap();
        provider.fail_next_unknown();
        let unknown = state
            .click(
                &context,
                &first.page,
                &first_snapshot.document,
                &first_snapshot.elements[0].element_ref,
                None,
                &cancel,
            )
            .await
            .unwrap_err();
        assert_eq!(unknown.0.code, "outcome_unknown");
        // Another page's snapshot and navigation must not clear page A's state.
        assert!(
            state
                .snapshot(&context, &second.page, &cancel)
                .await
                .is_ok()
        );
        assert!(
            state
                .navigate(&context, &second.page, "https://example.invalid", &cancel)
                .await
                .is_ok()
        );
        assert_eq!(
            state
                .navigate(&context, &first.page, "https://example.invalid", &cancel)
                .await
                .unwrap_err()
                .0
                .code,
            "outcome_unknown"
        );
        // Only a snapshot of page A (or close) restores it.
        assert!(state.snapshot(&context, &first.page, &cancel).await.is_ok());
        assert!(
            state
                .navigate(&context, &first.page, "https://example.invalid", &cancel)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn concurrent_cross_page_actions_do_not_clear_each_others_unknown() {
        let provider = Arc::new(FixtureProvider::full().with_delay(10));
        let run = make_run();
        let state = Arc::new(make_state(provider.clone(), &run));
        let context = make_context(run);
        let cancel = NeverCancel;
        let first = state.open(&context, &cancel).await.unwrap();
        let second = state.open(&context, &cancel).await.unwrap();
        let first_snapshot = state
            .snapshot(&context, &first.page, &cancel)
            .await
            .unwrap();
        state
            .snapshot(&context, &second.page, &cancel)
            .await
            .unwrap();
        provider.fail_next_unknown();
        let (unknown, other) = tokio::join!(
            state.click(
                &context,
                &first.page,
                &first_snapshot.document,
                &first_snapshot.elements[0].element_ref,
                None,
                &cancel,
            ),
            state.snapshot(&context, &second.page, &cancel)
        );
        assert_eq!(unknown.unwrap_err().0.code, "outcome_unknown");
        assert!(other.is_ok());
        assert_eq!(
            state
                .navigate(&context, &first.page, "https://example.invalid", &cancel)
                .await
                .unwrap_err()
                .0
                .code,
            "outcome_unknown"
        );
        assert!(
            state
                .navigate(&context, &second.page, "https://example.invalid", &cancel)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn unknown_open_blocks_all_old_pages_until_close() {
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = make_state(provider.clone(), &run);
        let context = make_context(run);
        let cancel = NeverCancel;
        let opened = state.open(&context, &cancel).await.unwrap();
        state
            .snapshot(&context, &opened.page, &cancel)
            .await
            .unwrap();
        provider.fail_next_unknown();
        assert_eq!(
            state.open(&context, &cancel).await.unwrap_err().0.code,
            "outcome_unknown"
        );
        for error in [
            state
                .snapshot(&context, &opened.page, &cancel)
                .await
                .unwrap_err(),
            state
                .navigate(&context, &opened.page, "https://example.invalid", &cancel)
                .await
                .unwrap_err(),
            state.open(&context, &cancel).await.unwrap_err(),
        ] {
            assert_eq!(error.0.code, "outcome_unknown");
        }
        state.close(&context, &cancel).await.unwrap();
        assert!(state.open(&context, &cancel).await.is_ok());
    }

    #[tokio::test]
    async fn wait_invalidates_document_and_actions_require_its_token() {
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = make_state(provider, &run);
        let context = make_context(run);
        let cancel = NeverCancel;
        let opened = state.open(&context, &cancel).await.unwrap();
        let snapshot = state
            .snapshot(&context, &opened.page, &cancel)
            .await
            .unwrap();
        let waited = state
            .wait(&context, &opened.page, 1, &cancel)
            .await
            .unwrap();
        assert!(waited.document_stale);
        assert_eq!(
            state
                .click(
                    &context,
                    &opened.page,
                    &snapshot.document,
                    &snapshot.elements[0].element_ref,
                    None,
                    &cancel,
                )
                .await
                .unwrap_err()
                .0
                .code,
            "stale_element"
        );
        let fresh = state
            .snapshot(&context, &opened.page, &cancel)
            .await
            .unwrap();
        assert_eq!(
            state
                .click(
                    &context,
                    &opened.page,
                    &snapshot.document,
                    &fresh.elements[0].element_ref,
                    None,
                    &cancel,
                )
                .await
                .unwrap_err()
                .0
                .code,
            "stale_element"
        );
    }

    #[tokio::test]
    async fn cancellation_interrupts_slow_wait_and_navigate() {
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = make_state(provider.clone(), &run);
        let context = make_context(run);
        let opened = state.open(&context, &NeverCancel).await.unwrap();
        state
            .snapshot(&context, &opened.page, &NeverCancel)
            .await
            .unwrap();

        let flag = Arc::new(AtomicBool::new(false));
        let cancel = SwitchCancel(flag.clone());
        let trigger = flag.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            trigger.store(true, Ordering::Release);
        });
        let error = state
            .wait(&context, &opened.page, 5_000, &cancel)
            .await
            .unwrap_err();
        task.await.unwrap();
        assert_eq!(error.0.code, "cancelled");

        let provider = Arc::new(FixtureProvider::full().with_delay(500));
        let run = make_run();
        let state = make_state(provider, &run);
        let context = make_context(run);
        let opened = state.open(&context, &NeverCancel).await.unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let cancel = SwitchCancel(flag.clone());
        let trigger = flag.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            trigger.store(true, Ordering::Release);
        });
        let error = state
            .navigate(&context, &opened.page, "https://example.invalid", &cancel)
            .await
            .unwrap_err();
        task.await.unwrap();
        // A navigation cancelled after dispatch may have changed the page.
        assert_eq!(error.0.code, "outcome_unknown");
        assert_eq!(
            state
                .navigate(
                    &context,
                    &opened.page,
                    "https://example.invalid",
                    &NeverCancel
                )
                .await
                .unwrap_err()
                .0
                .code,
            "outcome_unknown"
        );
    }

    #[tokio::test]
    async fn close_known_failure_keeps_pages_and_is_retryable() {
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = make_state(provider.clone(), &run);
        let context = make_context(run);
        let cancel = NeverCancel;
        let opened = state.open(&context, &cancel).await.unwrap();
        state
            .snapshot(&context, &opened.page, &cancel)
            .await
            .unwrap();
        provider.fail_next_close_known();
        let error = state.close(&context, &cancel).await.unwrap_err();
        assert_eq!(error.0.code, "close_failed");
        // A known close failure must not discard the page state.
        assert!(
            state
                .snapshot(&context, &opened.page, &cancel)
                .await
                .is_ok()
        );
        state.close(&context, &cancel).await.unwrap();
        assert_eq!(
            state
                .snapshot(&context, &opened.page, &cancel)
                .await
                .unwrap_err()
                .0
                .code,
            "browser_closed"
        );
        assert!(state.open(&context, &cancel).await.is_ok());
    }

    #[tokio::test]
    async fn shutdown_is_idempotent_and_retryable_after_failure() {
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = make_state(provider.clone(), &run);
        let context = make_context(run);
        let cancel = NeverCancel;
        let opened = state.open(&context, &cancel).await.unwrap();
        state
            .snapshot(&context, &opened.page, &cancel)
            .await
            .unwrap();
        provider.fail_next_shutdown_unknown();
        let error = state.shutdown(&cancel).await.unwrap_err();
        assert_eq!(error.0.code, "outcome_unknown");
        state.shutdown(&cancel).await.unwrap();
        state.shutdown(&cancel).await.unwrap();
        assert_eq!(provider.shutdown_count(), 1);
    }

    #[tokio::test]
    async fn open_after_shutdown_starts_a_new_provider_lifecycle() {
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = make_state(provider.clone(), &run);
        let context = make_context(run);
        let cancel = NeverCancel;
        let opened = state.open(&context, &cancel).await.unwrap();
        state
            .snapshot(&context, &opened.page, &cancel)
            .await
            .unwrap();
        state.shutdown(&cancel).await.unwrap();
        let fresh = state.open(&context, &cancel).await.unwrap();
        assert_eq!(
            state
                .snapshot(&context, &opened.page, &cancel)
                .await
                .unwrap_err()
                .0
                .code,
            "invalid_page"
        );
        assert!(state.snapshot(&context, &fresh.page, &cancel).await.is_ok());
    }

    #[tokio::test]
    async fn cancelled_open_after_shutdown_requires_provider_cleanup_again() {
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = make_state(provider.clone(), &run);
        let context = make_context(run);
        state.shutdown(&NeverCancel).await.unwrap();
        assert_eq!(provider.shutdown_count(), 1);

        provider.stall_next_open();
        let flag = Arc::new(AtomicBool::new(false));
        let cancel = SwitchCancel(flag.clone());
        let trigger = flag.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            trigger.store(true, Ordering::Release);
        });
        let error = state.open(&context, &cancel).await.unwrap_err();
        task.await.unwrap();
        assert_eq!(error.0.code, "outcome_unknown");
        // A cancelled open after shutdown invalidates the old clean fast path.
        state.shutdown(&NeverCancel).await.unwrap();
        assert_eq!(provider.shutdown_count(), 2);
    }

    #[tokio::test]
    async fn known_open_failure_after_shutdown_does_not_skip_cleanup() {
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = make_state(provider.clone(), &run);
        let context = make_context(run);
        state.shutdown(&NeverCancel).await.unwrap();
        assert_eq!(provider.shutdown_count(), 1);

        provider.fail_next_open_known();
        let error = state.open(&context, &NeverCancel).await.unwrap_err();
        assert_eq!(error.0.code, "open_failed");
        state.shutdown(&NeverCancel).await.unwrap();
        assert_eq!(provider.shutdown_count(), 2);
    }

    #[tokio::test]
    async fn cancelled_dispatched_actions_report_unknown_and_block_replay() {
        #[derive(Clone, Copy)]
        enum Action {
            Navigate,
            Click,
            Fill,
            Press,
            Upload,
            Download,
        }
        for action in [
            Action::Navigate,
            Action::Click,
            Action::Fill,
            Action::Press,
            Action::Upload,
            Action::Download,
        ] {
            let provider = Arc::new(FixtureProvider::full());
            let run = make_run();
            let state = make_state(provider.clone(), &run);
            let context = make_context(run);
            let opened = state.open(&context, &NeverCancel).await.unwrap();
            let snapshot = state
                .snapshot(&context, &opened.page, &NeverCancel)
                .await
                .unwrap();
            let document = snapshot.document.clone();
            let element = snapshot.elements[0].element_ref.clone();

            provider.stall_next_action();
            let flag = Arc::new(AtomicBool::new(false));
            let cancel = SwitchCancel(flag.clone());
            let trigger = flag.clone();
            let task = tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                trigger.store(true, Ordering::Release);
            });
            let error = match action {
                Action::Navigate => state
                    .navigate(&context, &opened.page, "https://example.invalid", &cancel)
                    .await
                    .unwrap_err(),
                Action::Click => state
                    .click(&context, &opened.page, &document, &element, None, &cancel)
                    .await
                    .unwrap_err(),
                Action::Fill => state
                    .fill(&context, &opened.page, &document, &element, "text", &cancel)
                    .await
                    .unwrap_err(),
                Action::Press => state
                    .press(&context, &opened.page, &document, "Enter", &cancel)
                    .await
                    .unwrap_err(),
                Action::Upload => state
                    .upload(
                        &context,
                        &opened.page,
                        &document,
                        &element,
                        "artifact_1",
                        &cancel,
                    )
                    .await
                    .unwrap_err(),
                Action::Download => state
                    .download(&context, &opened.page, &cancel)
                    .await
                    .unwrap_err(),
            };
            task.await.unwrap();
            assert_eq!(error.0.code, "outcome_unknown");
            assert_eq!(
                state
                    .navigate(
                        &context,
                        &opened.page,
                        "https://example.invalid",
                        &NeverCancel
                    )
                    .await
                    .unwrap_err()
                    .0
                    .code,
                "outcome_unknown"
            );
        }
    }

    #[tokio::test]
    async fn provider_truncated_snapshot_is_reported_and_propagated() {
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = make_state(provider.clone(), &run);
        let context = make_context(run);
        let opened = state.open(&context, &NeverCancel).await.unwrap();
        provider.truncate_next_snapshot();
        let snapshot = state
            .snapshot(&context, &opened.page, &NeverCancel)
            .await
            .unwrap();
        assert!(snapshot.truncated);
    }

    #[tokio::test]
    async fn invalid_provider_metadata_fails_closed() {
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = make_state(provider.clone(), &run);
        let context = make_context(run);
        let opened = state.open(&context, &NeverCancel).await.unwrap();

        provider.return_next_action_url("not a url");
        assert_eq!(
            state
                .navigate(
                    &context,
                    &opened.page,
                    "https://example.invalid",
                    &NeverCancel
                )
                .await
                .unwrap_err()
                .0
                .code,
            "invalid_provider_result"
        );
        provider.return_next_action_url(format!("https://example.invalid/{}", "a".repeat(9000)));
        assert_eq!(
            state
                .navigate(
                    &context,
                    &opened.page,
                    "https://example.invalid",
                    &NeverCancel
                )
                .await
                .unwrap_err()
                .0
                .code,
            "invalid_provider_result"
        );
        provider.return_next_action_url("not a url");
        assert_eq!(
            state
                .wait(&context, &opened.page, 0, &NeverCancel)
                .await
                .unwrap_err()
                .0
                .code,
            "invalid_provider_result"
        );
        provider.return_next_snapshot_title("bad\u{0}title");
        assert_eq!(
            state
                .snapshot(&context, &opened.page, &NeverCancel)
                .await
                .unwrap_err()
                .0
                .code,
            "invalid_provider_result"
        );
    }

    #[tokio::test]
    async fn state_rejects_other_runs() {
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = make_state(provider, &run);
        assert_eq!(
            state
                .open(
                    &make_context(Run {
                        id: RunId::new(),
                        session_id: crate::contract::SessionId::new(),
                        work_mode: WorkMode::Work,
                        permissions: PermissionMode::Auto,
                    }),
                    &NeverCancel,
                )
                .await
                .unwrap_err()
                .0
                .code,
            "invalid_run"
        );
    }
}
