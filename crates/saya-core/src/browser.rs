//! Host-owned browser capability boundary.
//!
//! `BrowserProvider` is the only way the runtime reaches a browser worker.
//! `BrowserState` isolates one private session per Run, serializes actions per
//! page, and maps provider element keys to random opaque model-visible
//! references. It never exposes selectors, scripts, evaluate, install, raw MCP
//! discovery or developer tools. Unknown dispatched actions stop replay until a
//! fresh snapshot confirms an observable state.

pub mod artifacts;
pub mod credentials;
pub mod network;
pub mod settings;
pub mod vm;
pub mod worker;

use crate::{
    background::BackgroundJobs,
    boundary::{Cancellation, Operation, Tool, ToolContext, ToolDefinition},
    contract::{BoundaryError, ErrorInfo, Run, RunId, SessionId, ToolCall, ToolResult},
    dispatch::ToolDispatcher,
    output_store::{
        CaptureCompletion, CapturedToolResult, OutputMetadata, OutputScope, OutputStore,
        OutputStream, SealedMedia, SealedOutput,
    },
};
use artifacts::BrowserArtifactStore;
use credentials::BrowserCredentialBroker;
use serde::{Deserialize, Serialize};
pub use settings::{
    BrowserCredentialPurpose, BrowserCredentialSettings, BrowserCredentialSlot, BrowserFileSettings,
};
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::sync::{Mutex, atomic::AtomicU64};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Mutex as AsyncMutex, watch};

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
    value.map_or(Ok(()), |url| {
        provider_url(url)?;
        provider_final_url(url).map(|_| ())
    })
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

/// Classification of a provider-reported final URL.
///
/// Redirects and OAuth-style flows legitimately append a fragment, so the
/// final URL check allows one while still requiring HTTP(S), no user-info and
/// a valid host. `about:blank` and Chromium's own error page are explicit
/// provider states with no network origin; they are never treated as malformed
/// and never participate in an origin cross-check.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProviderFinalUrl {
    Origin(crate::network::NormalizedOrigin),
    Blank,
    BrowserError,
}

fn provider_final_url(value: &str) -> Result<ProviderFinalUrl, BrowserFailure> {
    let invalid = || {
        invalid_provider_result("Browser provider returned an invalid final URL outside HTTP(S)")
    };
    // Parse the scheme first so `about:blank` is matched on its path instead
    // of the raw string: Chromium may append a legal query or fragment.
    let parsed = reqwest::Url::parse(value).map_err(|_| invalid())?;
    match parsed.scheme() {
        "about" => {
            if parsed.path().eq_ignore_ascii_case("blank") {
                Ok(ProviderFinalUrl::Blank)
            } else {
                Err(invalid())
            }
        }
        // Chromium's own network error page is an explicit provider state.
        // It never represents a reachable network origin and takes no part in
        // an origin cross-check.
        "chrome-error" => Ok(ProviderFinalUrl::BrowserError),
        // HTTP(S) keeps every strict rule: no user-info, a valid host and the
        // same character restrictions. A fragment is tolerated for redirect
        // and OAuth-style flows.
        "http" | "https" => crate::network::normalize_final_target(value)
            .map(|target| ProviderFinalUrl::Origin(target.origin))
            .map_err(|_| invalid()),
        _ => Err(invalid()),
    }
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

/// Default-disabled browser settings. The optional worker settings identify a
/// host-provisioned Playwright runtime; enabling them does not install or
/// download anything.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrowserSettings {
    pub enabled: bool,
    pub max_snapshot_bytes: usize,
    pub max_wait_ms: u64,
    pub worker: worker::BrowserWorkerSettings,
    /// Explicit dedicated-VM worker. Selecting it never falls back to the
    /// native/Seatbelt/Host backends.
    pub vm: vm::BrowserVmSettings,
    /// Strict browser network policy. The VM backend never uses the VM's
    /// default egress; it requires this policy and a host broker.
    pub network: network::BrowserNetworkSettings,
    /// Origin-bound fill slots. The model may name only an alias.
    pub credentials: BrowserCredentialSettings,
    /// Bounded download/upload file broker.
    pub files: BrowserFileSettings,
}
impl Default for BrowserSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            max_snapshot_bytes: DEFAULT_MAX_SNAPSHOT_BYTES,
            max_wait_ms: DEFAULT_MAX_WAIT_MS,
            worker: worker::BrowserWorkerSettings::default(),
            vm: vm::BrowserVmSettings::default(),
            network: network::BrowserNetworkSettings::default(),
            credentials: BrowserCredentialSettings::default(),
            files: BrowserFileSettings::default(),
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
        self.worker.validate()?;
        self.vm.validate()?;
        self.network.validate()?;
        self.credentials.validate()?;
        self.files.validate()?;
        if self.vm.enabled && self.worker.enabled {
            return Err(invalid_config(
                "Browser must select either the native worker or the VM backend, not both",
            ));
        }
        if self.vm.enabled && !self.network.enabled {
            return Err(invalid_config(
                "The VM browser backend requires an explicit browser network policy",
            ));
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
    /// A screenshot may be exposed only when the provider can mask passwords,
    /// one-time codes, credential-slot elements and marked sensitive inputs
    /// before capture. A provider that cannot do this must leave both flags
    /// false and return `browser_capability_disabled`.
    pub screenshot_masking: bool,
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
    /// Frame identity when the provider reports this element inside a child
    /// frame. Main-document elements leave it empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<String>,
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
    /// True only when the provider masked passwords, one-time codes,
    /// credential-slot elements and marked sensitive inputs before capture.
    pub masked: bool,
    /// Observed top-level page URL at capture time, when the provider reports it.
    pub url: Option<String>,
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

/// A host-staged upload file. Providers copy it into their own Run-private
/// worker staging area; they never accept a model-supplied host path.
#[derive(Debug, Clone)]
pub struct BrowserUploadSource {
    pub filename: String,
    pub path: std::path::PathBuf,
    pub sha256: String,
    pub size: u64,
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
    /// Perform one explicit element action and return only a download produced
    /// by that action. The provider must not treat this as a generic "last
    /// download" lookup.
    async fn download(
        &self,
        _session: &str,
        _page: &str,
        _element: &str,
        _cancel: &dyn Cancellation,
    ) -> Result<BrowserDownload, BrowserFailure> {
        Err(BrowserFailure::unsupported("download"))
    }
    /// Open the element's file chooser and upload exactly one staged file.
    async fn upload(
        &self,
        _session: &str,
        _page: &str,
        _element: &str,
        _upload: &crate::browser::BrowserUploadSource,
        _cancel: &dyn Cancellation,
    ) -> Result<BrowserUpload, BrowserFailure> {
        Err(BrowserFailure::unsupported("upload"))
    }
    async fn shutdown(&self, _cancel: &dyn Cancellation) -> Result<(), BrowserFailure>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ElementTarget {
    key: String,
    /// Present only when the element lives inside a child frame.
    frame: Option<String>,
}

#[derive(Default)]
struct PageDocument {
    token: Option<String>,
    elements: BTreeMap<String, ElementTarget>,
    revision: u64,
    /// Exact canonical origin observed with the current document token.
    origin: Option<crate::network::NormalizedOrigin>,
    /// Last provider-confirmed URL for this page. Used only for host metadata
    /// and never as an authorization decision.
    url: Option<String>,
    /// An action on this page may have been dispatched without a confirmed
    /// outcome. Only a fresh snapshot of this page, or a complete close, clears
    /// it. It must never be cleared by another page's snapshot or a new open.
    uncertain: bool,
}
impl PageDocument {
    fn invalidate(&mut self) {
        self.token = None;
        self.elements.clear();
        self.origin = None;
        self.url = None;
        self.revision = self.revision.saturating_add(1);
    }
    fn fail_unknown(&mut self) {
        self.uncertain = true;
        self.invalidate();
    }
    fn observe(
        &mut self,
        token: String,
        elements: BTreeMap<String, ElementTarget>,
        origin: Option<crate::network::NormalizedOrigin>,
        url: String,
    ) {
        self.uncertain = false;
        self.token = Some(token);
        self.elements = elements;
        self.origin = origin;
        self.url = Some(url);
        self.revision = self.revision.saturating_add(1);
    }
    fn confirm_action(&mut self, action: &BrowserAction) -> Result<(), BrowserFailure> {
        provider_optional_url(action.url.as_deref())
            .and_then(|()| provider_optional_detail(action.detail.as_deref()))?;
        // Metadata only: an action never establishes document authorization.
        self.url = action.url.clone();
        Ok(())
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
    credentials: Option<BrowserCredentialBroker>,
    artifacts: Option<BrowserArtifactStore>,
    /// The profile image capability observed when this Run was configured.
    /// Screenshot metadata reports this declaration, never a per-request fact.
    model_image_capability: bool,
    /// Weak supervisor handle prevents the
    /// BrowserState → BackgroundJobs → dispatcher → BrowserTool → BrowserState
    /// ownership cycle. The binding is retained across PLAN cleanup and
    /// re-armed by an explicit `open`; a closed supervisor is never reused.
    background: std::sync::Mutex<Option<std::sync::Weak<BackgroundJobs>>>,
    /// True only while a usable provider/page lifecycle exists. PLAN cleanup
    /// disables background starts until a later explicit `open`.
    background_enabled: AtomicBool,
    /// Per-lifecycle cancellation observed by every provider call. PLAN
    /// cleanup or Run shutdown signals it so an in-flight `wait` releases its
    /// page lock instead of blocking cleanup for its full deadline.
    lifecycle: watch::Sender<bool>,
    run_id: RunId,
    session_id: SessionId,
}
/// Cancellation observed by provider calls inside one browser lifecycle. The
/// external caller cancellation and the state's own shutdown signal both wake
/// it, so PLAN cleanup can interrupt an in-flight `wait` or navigation instead
/// of waiting for its page lock to be released by the deadline.
struct LifecycleCancellation<'a> {
    outer: &'a dyn Cancellation,
    lifecycle: watch::Receiver<bool>,
}
impl Cancellation for LifecycleCancellation<'_> {
    fn is_cancelled(&self) -> bool {
        self.outer.is_cancelled() || *self.lifecycle.borrow()
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        let outer = self.outer;
        let mut lifecycle = self.lifecycle.clone();
        Box::pin(async move {
            tokio::select! {
                biased;
                () = outer.cancelled() => (),
                () = async move {
                    while !*lifecycle.borrow_and_update() {
                        if lifecycle.changed().await.is_err() {
                            break;
                        }
                    }
                } => (),
            }
        })
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_slot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_uses_remaining: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotView {
    /// Page token the screenshot was taken from.
    pub page: String,
    /// Last provider-confirmed URL, not an observation at capture time.
    /// The page may navigate autonomously between provider confirmations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Explicitly qualifies `url`; capture itself does not observe a URL.
    pub url_observation: String,
    pub document_revision: u64,
    pub mime: String,
    pub filename: String,
    pub width: u32,
    pub height: u32,
    pub bytes: usize,
    pub sha256: String,
    /// Sensitive fields and credential slots were masked before capture.
    pub masked: bool,
    #[serde(skip)]
    pub raw_bytes: Vec<u8>,
    /// Host-only durable media reference. Providers never receive it; the
    /// interface uses `artifact_ref`.
    #[serde(skip_serializing)]
    pub media_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<String>,
    /// The selected profile's image capability when this screenshot was taken.
    /// This is a capability declaration, not a per-request injection fact.
    pub model_image_capability: String,
    /// Explains whether the saved image can be offered to the model.
    pub model_image_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadView {
    pub filename: String,
    pub mime: String,
    pub bytes: usize,
    pub sha256: String,
    pub artifact_stored: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<String>,
    /// Host-only durable reference; the interface and model use `artifact_ref`.
    #[serde(skip_serializing)]
    pub media_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadView {
    pub filename: String,
    pub bytes_sent: u64,
    pub document_revision: u64,
    pub artifact_ref: String,
    pub sha256: String,
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
            credentials: None,
            artifacts: None,
            model_image_capability: false,
            background: std::sync::Mutex::new(None),
            background_enabled: AtomicBool::new(false),
            lifecycle: watch::channel(false).0,
            run_id: run.id,
            session_id: run.session_id,
        })
    }

    /// Construct a state with all host-owned services for one Run. Credential
    /// slots are opened from their private store, File artifacts use the Run's
    /// OutputStore, and slow actions share the existing background supervisor.
    pub fn new_with_services(
        settings: BrowserSettings,
        provider: Arc<dyn BrowserProvider>,
        run: Run,
        outputs: Option<Arc<OutputStore>>,
        jobs: Option<Arc<BackgroundJobs>>,
        registry: Arc<crate::credentials::SecretRegistry>,
        media_capabilities: crate::config::MediaCapabilities,
    ) -> Result<Self, BoundaryError> {
        let mut state = Self::new(settings.clone(), provider, run)?;
        state.model_image_capability = media_capabilities.image;
        if settings.credentials.enabled {
            let broker = BrowserCredentialBroker::from_settings(&settings.credentials, registry)?;
            state = state.with_credentials(broker);
        }
        if settings.files.enabled
            && let Some(outputs) = outputs
        {
            state = state.with_artifacts(outputs)?;
        }
        if let Some(jobs) = jobs {
            state = state.with_background_jobs(jobs);
        }
        Ok(state)
    }

    /// Screenshots need a masking provider plus the bounded file broker and
    /// artifact store. Registration and execution share this exact predicate.
    pub(crate) fn screenshot_available(&self) -> bool {
        self.facts.screenshots
            && self.facts.screenshot_masking
            && self.settings.files.enabled
            && self.artifacts.is_some()
    }

    /// Declare whether the selected profile can accept images. Screenshot
    /// metadata reports this declaration; it is not a per-request fact.
    pub fn with_model_image_capability(mut self, enabled: bool) -> Self {
        self.model_image_capability = enabled;
        self
    }

    /// Attach origin-bound credential slots. The broker is host-owned and never
    /// constructed from model input.
    pub(crate) fn with_credentials(mut self, broker: BrowserCredentialBroker) -> Self {
        self.credentials = Some(broker);
        self
    }

    /// Enable the private file broker. Files are stored in this Run's
    /// OutputStore and staged under its protected directory.
    pub(crate) fn with_artifacts(mut self, store: Arc<OutputStore>) -> Result<Self, BoundaryError> {
        self.artifacts = Some(BrowserArtifactStore::new(store, self.session_id)?);
        Ok(self)
    }

    /// Attach the Run's existing background supervisor so slow browser actions
    /// use the same limits, cancellation, PLAN handling and cleanup. The weak
    /// binding survives PLAN cleanup; an explicit `open` re-arms it unless the
    /// supervisor has already closed.
    pub(crate) fn with_background_jobs(mut self, jobs: Arc<BackgroundJobs>) -> Self {
        self.background = std::sync::Mutex::new(Some(Arc::downgrade(&jobs)));
        self.background_enabled.store(true, Ordering::Release);
        self
    }

    /// Re-arm the shared background binding for a fresh provider lifecycle.
    /// Idempotent; a closed or dropped supervisor is never reused.
    fn rebind_background(&self) {
        let Ok(mut background) = self.background.lock() else {
            self.background_enabled.store(false, Ordering::Release);
            return;
        };
        let live = background
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .is_some_and(|jobs| jobs.is_closed().is_ok_and(|closed| !closed));
        if live {
            self.background_enabled.store(true, Ordering::Release);
        } else {
            *background = None;
            self.background_enabled.store(false, Ordering::Release);
        }
    }

    fn observe<'a>(&'a self, cancel: &'a dyn Cancellation) -> LifecycleCancellation<'a> {
        LifecycleCancellation {
            outer: cancel,
            lifecycle: self.lifecycle.subscribe(),
        }
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
        // Re-arm the per-lifecycle cancellation and background binding. This
        // is what makes `background=true` usable again after PLAN cleanup.
        self.lifecycle.send_replace(false);
        self.rebind_background();
        let cancel = self.observe(cancel);
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
            result = self.provider.open(session.as_deref(), &cancel) => result,
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
                        document: AsyncMutex::new(PageDocument {
                            url: opened.url.clone(),
                            ..PageDocument::default()
                        }),
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
        let cancel = self.observe(cancel);
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
            Some(session) => self.provider.close(session, &cancel).await,
            // An unknown open may have created provider resources with no
            // confirmed session identity. Only a full provider shutdown can
            // establish that the provider is clean.
            None if open_uncertain => self.provider.shutdown(&cancel).await,
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
                self.background_enabled.store(false, Ordering::Release);
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
        let cancel = self.observe(cancel);
        let target = crate::network::normalize_target(url).map_err(|_| {
            failure(
                "invalid_args",
                "Browser navigation requires an explicit HTTP(S) URL",
            )
        })?;
        if self.settings.network.enabled && !self.settings.network.allows_origin(&target.origin) {
            return Err(failure(
                "browser_network_denied",
                "Browser navigation target is outside the Run's allowed origins",
            ));
        }
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
            result = self.provider.navigate(&session, &page.key, url, &cancel) => result,
        };
        document.invalidate();
        match result {
            Ok(action) => {
                if let Err(error) = provider_optional_url(action.url.as_deref())
                    .and_then(|()| provider_optional_detail(action.detail.as_deref()))
                {
                    return Err(error.into_boundary());
                }
                // The broker checks every redirect hop, including ones inside
                // an opaque CONNECT tunnel. Cross-check the worker-reported
                // final URL only when this Run actually has a browser network
                // policy; the native backend runs with `network.enabled =
                // false` and must not apply an unrelated origin check.
                if let Some(final_url) = action.url.as_deref() {
                    match provider_final_url(final_url) {
                        Ok(ProviderFinalUrl::Origin(origin)) => {
                            if self.settings.network.enabled
                                && !self.settings.network.allows_origin(&origin)
                            {
                                return Err(failure(
                                    "network_final_origin_denied",
                                    "Browser navigation ended at an origin outside the Run's allowed origins",
                                ));
                            }
                        }
                        // Internal blank pages and the browser's own error page
                        // are explicit non-network states.
                        Ok(ProviderFinalUrl::Blank | ProviderFinalUrl::BrowserError) => {}
                        Err(error) => return Err(error.into_boundary()),
                    }
                }
                document.url = action.url.clone();
                let final_url = action.url.unwrap_or_else(|| url.to_owned());
                Ok(NavigateView {
                    page: page_id,
                    url: final_url,
                    document_stale: true,
                })
            }
            Err(error) => {
                if error.uncertain {
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
        let cancel = self.observe(cancel);
        let page_id = page.to_owned();
        let (session, page) = self.active_page(context, page).await?;
        let mut document = page.document.lock().await;
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(failure("cancelled", "Browser snapshot cancelled")),
            result = self
                .provider
                .snapshot(&session, &page.key, self.settings.max_snapshot_bytes, &cancel) => result,
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
            element_map.insert(
                reference.clone(),
                ElementTarget {
                    key: element.key.clone(),
                    frame: element.frame.clone(),
                },
            );
            elements.push(ElementView {
                element_ref: reference,
                role,
                name,
            });
        }
        if snapshot.elements.len() > elements.len() {
            truncated = true;
        }
        let origin = match provider_final_url(&snapshot.url) {
            Ok(ProviderFinalUrl::Origin(origin)) => Some(origin),
            _ => None,
        };
        document.observe(token.clone(), element_map, origin, snapshot.url.clone());
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
    ) -> Result<ElementTarget, BoundaryError> {
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
        let cancel = self.observe(cancel);
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
                .click(&session, &page_state.key, &provider_element.key, button, &cancel) => result,
        };
        state.invalidate();
        let revision = state.revision;
        match result {
            Ok(action) => {
                if let Err(error) = state.confirm_action(&action) {
                    state.fail_unknown();
                    return Err(error.into_boundary());
                }
                Ok(ActionResultView {
                    page: page_id,
                    url: action.url,
                    detail: action.detail,
                    document_stale: true,
                    document_revision: revision,
                    credential_slot: None,
                    credential_uses_remaining: None,
                })
            }
            Err(error) => {
                if error.uncertain {
                    state.fail_unknown();
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
        let cancel = self.observe(cancel);
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
                .fill(&session, &page_state.key, &provider_element.key, text, &cancel) => result,
        };
        state.invalidate();
        let revision = state.revision;
        match result {
            Ok(action) => {
                if let Err(error) = state.confirm_action(&action) {
                    state.fail_unknown();
                    return Err(error.into_boundary());
                }
                Ok(ActionResultView {
                    page: page_id,
                    url: action.url,
                    detail: action.detail,
                    document_stale: true,
                    document_revision: revision,
                    credential_slot: None,
                    credential_uses_remaining: None,
                })
            }
            Err(error) => {
                if error.uncertain {
                    state.fail_unknown();
                }
                Err(error.into_boundary())
            }
        }
    }

    /// Fill one credential slot. The slot alias is resolved and consumed only
    /// after the current document token, exact page origin, purpose,
    /// revocation and remaining uses are verified. Plaintext exists only at
    /// the provider call boundary and is never placed in the result.
    pub async fn fill_credential(
        &self,
        context: &ToolContext,
        page: &str,
        document: &str,
        element: &str,
        slot: &str,
        cancel: &dyn Cancellation,
    ) -> Result<ActionResultView, BoundaryError> {
        let cancel = self.observe(cancel);
        let page_id = page.to_owned();
        let (session, page_state) = self.active_page(context, page).await?;
        let mut state = page_state.document.lock().await;
        let provider_element = Self::verify_document(&state, document, element)?;
        // Credential material must never cross a frame boundary. The exact
        // origin observed for this document belongs to the top-level page, so
        // a child frame could be same-origin by URL but still isolate the
        // injected value from the origin check. Reject every subframe target.
        if provider_element.frame.is_some() {
            return Err(failure(
                "credential_frame_denied",
                "Browser credential fill is limited to the top-level document",
            ));
        }
        let origin = state.origin.clone().ok_or_else(|| {
            failure(
                "credential_origin_mismatch",
                "Browser page has no verified origin for credential fill",
            )
        })?;
        let broker = self.credentials.as_ref().ok_or_else(|| {
            failure(
                "credential_unavailable",
                "Browser credential slots are not configured",
            )
        })?;
        let resolved = broker
            .resolve(slot, &origin)
            .await
            .map_err(|error| error.into_boundary())?;
        if cancel.is_cancelled() {
            return Err(failure("cancelled", "Browser credential fill cancelled"));
        }
        // Consume before dispatch: a dispatched fill whose outcome is unknown
        // may already have exposed the value and must not be replayed.
        let remaining = broker
            .consume(slot)
            .map_err(|error| error.into_boundary())?;
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                state.fail_unknown();
                return Err(BrowserFailure::uncertain(
                    "Browser credential fill was cancelled after dispatch",
                )
                .into_boundary());
            }
            result = self
                .provider
                .fill(&session, &page_state.key, &provider_element.key, resolved.expose(), &cancel) => result,
        };
        state.invalidate();
        let revision = state.revision;
        match result {
            Ok(action) => {
                if let Err(error) = state.confirm_action(&action) {
                    state.fail_unknown();
                    return Err(error.into_boundary());
                }
                Ok(ActionResultView {
                    page: page_id,
                    url: action.url,
                    detail: action.detail,
                    document_stale: true,
                    document_revision: revision,
                    credential_slot: Some(resolved.alias.clone()),
                    credential_uses_remaining: Some(remaining),
                })
            }
            Err(error) => {
                if error.uncertain {
                    state.fail_unknown();
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
        let cancel = self.observe(cancel);
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
            result = self.provider.press(&session, &page.key, key, &cancel) => result,
        };
        state.invalidate();
        let revision = state.revision;
        match result {
            Ok(action) => {
                if let Err(error) = state.confirm_action(&action) {
                    state.fail_unknown();
                    return Err(error.into_boundary());
                }
                Ok(ActionResultView {
                    page: page_id,
                    url: action.url,
                    detail: action.detail,
                    document_stale: true,
                    document_revision: revision,
                    credential_slot: None,
                    credential_uses_remaining: None,
                })
            }
            Err(error) => {
                if error.uncertain {
                    state.fail_unknown();
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
        let cancel = self.observe(cancel);
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
            result = self.provider.wait(&session, &page.key, milliseconds, &cancel) => result,
        };
        // Waiting never confirms a document revision: conservatively invalidate.
        state.invalidate();
        let revision = state.revision;
        match result {
            Ok(action) => {
                state
                    .confirm_action(&action)
                    .map_err(BrowserFailure::into_boundary)?;
                Ok(ActionResultView {
                    page: page_id,
                    url: action.url,
                    detail: action.detail,
                    document_stale: true,
                    document_revision: revision,
                    credential_slot: None,
                    credential_uses_remaining: None,
                })
            }
            Err(error) => {
                if error.uncertain {
                    state.fail_unknown();
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
    ) -> Result<(ScreenshotView, Option<SealedMedia>), BoundaryError> {
        if !self.screenshot_available() {
            return Err(failure(
                "browser_capability_disabled",
                "Browser screenshot requires a masking provider plus the bounded file broker and artifact store",
            ));
        }
        let cancel = self.observe(cancel);
        let page_token = page.to_owned();
        let (session, page) = self.active_page(context, page).await?;
        let mut state = page.document.lock().await;
        if state.uncertain {
            return Err(failure(
                "outcome_unknown",
                "This page's outcome is unknown; take a fresh snapshot first",
            ));
        }
        let document_revision = state.revision;
        let url = state.url.clone();
        let page_id = page_token;
        let screenshot = match tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(failure("cancelled", "Browser screenshot cancelled")),
            result = self.provider.screenshot(&session, &page.key, &cancel) => result,
        } {
            Ok(screenshot) => screenshot,
            Err(error) => {
                if error.uncertain {
                    state.fail_unknown();
                }
                return Err(error.into_boundary());
            }
        };
        if let Err(error) = provider_field(
            &screenshot.mime,
            MAX_PROVIDER_MIME_BYTES,
            "Browser provider returned an invalid screenshot type",
        ) {
            return Err(error.into_boundary());
        }
        // An unmasked capture is never a successful screenshot: the host must
        // not persist or expose it.
        if !screenshot.masked {
            return Err(failure(
                "browser_capability_disabled",
                "Browser provider did not confirm sensitive-field masking",
            ));
        }
        if let Some(observed_url) = screenshot.url.as_deref() {
            if let Err(error) = provider_url(observed_url) {
                return Err(error.into_boundary());
            }
            match provider_final_url(observed_url) {
                Ok(ProviderFinalUrl::Origin(origin)) => {
                    if self.settings.network.enabled
                        && !self.settings.network.allows_origin(&origin)
                    {
                        state.invalidate();
                        return Err(failure(
                            "network_final_origin_denied",
                            "Browser screenshot observed an origin outside the Run's allowed origins",
                        ));
                    }
                }
                Ok(ProviderFinalUrl::Blank | ProviderFinalUrl::BrowserError) => {}
                Err(error) => return Err(error.into_boundary()),
            }
            if let Some(expected_url) = state.url.as_deref()
                && expected_url != observed_url
            {
                state.invalidate();
                return Err(failure(
                    "stale_document",
                    "Browser document URL changed before screenshot capture",
                ));
            }
        }
        let sha256 = digest_hex(&screenshot.bytes);
        let bytes = screenshot.bytes.len();
        let mime = screenshot.mime;
        let extension = match mime.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            other => {
                return Err(failure(
                    "invalid_provider_result",
                    format!("Browser screenshot type is not supported: {other}"),
                ));
            }
        };
        // The host, not the provider, chooses the stored file name.
        let filename = format!("screenshot-{}.{extension}", uuid::Uuid::now_v7().simple());
        let (stored, sealed) = crate::browser::artifacts::store_image_artifact(
            self.artifacts.as_ref().ok_or_else(|| {
                failure("browser_capability_disabled", "Image store is unavailable")
            })?,
            &filename,
            &mime,
            screenshot.width,
            screenshot.height,
            screenshot.bytes,
        )?;
        Ok((
            ScreenshotView {
                page: page_id,
                url,
                url_observation: "last_confirmed".into(),
                document_revision,
                sha256,
                bytes,
                raw_bytes: Vec::new(),
                mime,
                filename,
                width: screenshot.width,
                height: screenshot.height,
                masked: true,
                media_ref: Some(stored.reference.clone()),
                artifact_ref: Some(stored.reference),
                model_image_capability: if self.model_image_capability {
                    "enabled".into()
                } else {
                    "disabled".into()
                },
                model_image_status: if self.model_image_capability {
                    "artifact_saved: the selected profile declares image input enabled; the image is injected only if it remains in the next Execution request"
                } else {
                    "artifact_saved: the selected profile declares image input disabled; the image is not injected"
                }
                .into(),
            },
            Some(sealed),
        ))
    }

    pub async fn download(
        &self,
        context: &ToolContext,
        page: &str,
        document: &str,
        element: &str,
        cancel: &dyn Cancellation,
    ) -> Result<(DownloadView, Option<SealedMedia>), BoundaryError> {
        let cancel = self.observe(cancel);
        if !self.settings.files.enabled || self.artifacts.is_none() {
            return Err(failure(
                "unsupported",
                "Browser file broker is not configured",
            ));
        }
        let (session, page_state) = self.active_page(context, page).await?;
        let mut state = page_state.document.lock().await;
        let provider_element = Self::verify_document(&state, document, element)?;
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
            result = self
                .provider
                .download(&session, &page_state.key, &provider_element.key, &cancel) => result,
        } {
            Ok(download) => download,
            Err(error) => {
                // Every provider download error conservatively invalidates the
                // document. Known failures stay retryable after a fresh
                // snapshot; an unknown outcome keeps the page unknown.
                if error.uncertain {
                    state.fail_unknown();
                } else {
                    state.invalidate();
                }
                return Err(error.into_boundary());
            }
        };
        state.invalidate();
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
            state.invalidate();
            return Err(error.into_boundary());
        }
        let artifacts = self
            .artifacts
            .as_ref()
            .ok_or_else(|| failure("unsupported", "Browser file broker is not configured"))?;
        let (stored, sealed) = match artifacts.store_download(
            &download.filename,
            &download.mime,
            &download.bytes,
            self.settings.files.max_download_bytes,
        ) {
            Ok(stored) => stored,
            Err(error) => {
                state.invalidate();
                return Err(error);
            }
        };
        Ok((
            DownloadView {
                filename: stored.filename,
                mime: stored.mime,
                bytes: stored.size as usize,
                sha256: stored.sha256,
                artifact_stored: true,
                artifact_ref: Some(stored.reference.clone()),
                media_ref: Some(stored.reference),
            },
            Some(sealed),
        ))
    }

    pub async fn upload(
        &self,
        context: &ToolContext,
        page: &str,
        document: &str,
        element: &str,
        artifact_ref: &str,
        cancel: &dyn Cancellation,
    ) -> Result<UploadView, BoundaryError> {
        let cancel = self.observe(cancel);
        if !self.settings.files.enabled || self.artifacts.is_none() {
            return Err(failure(
                "unsupported",
                "Browser file broker is not configured",
            ));
        }
        let (session, page_state) = self.active_page(context, page).await?;
        let mut state = page_state.document.lock().await;
        let provider_element = Self::verify_document(&state, document, element)?;
        let artifacts = self
            .artifacts
            .as_ref()
            .ok_or_else(|| failure("unsupported", "Browser file broker is not configured"))?;
        // Only a Session-owned, digest-verified File artifact is accepted.
        let staged = artifacts.stage_upload(artifact_ref, self.settings.files.max_upload_bytes)?;
        if cancel.is_cancelled() {
            return Err(failure("cancelled", "Browser upload cancelled"));
        }
        let source = BrowserUploadSource {
            filename: staged.filename.clone(),
            path: staged.path.clone(),
            sha256: staged.sha256.clone(),
            size: staged.media_size(),
        };
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
                .upload(&session, &page_state.key, &provider_element.key, &source, &cancel) => result,
        };
        state.invalidate();
        let revision = state.revision;
        match result {
            Ok(upload) => {
                if let Err(error) = provider_field(
                    &upload.filename,
                    MAX_PROVIDER_FILENAME_BYTES,
                    "Browser provider returned an invalid upload filename",
                ) {
                    state.fail_unknown();
                    return Err(error.into_boundary());
                }
                if upload.bytes_sent != source.size {
                    state.fail_unknown();
                    return Err(failure(
                        "invalid_provider_result",
                        "Browser provider did not confirm the complete upload",
                    ));
                }
                let sha256 = staged.sha256.clone();
                Ok(UploadView {
                    filename: source.filename.clone(),
                    bytes_sent: upload.bytes_sent,
                    document_revision: revision,
                    artifact_ref: staged.reference.clone(),
                    sha256,
                })
            }
            Err(error) => {
                if error.uncertain {
                    state.fail_unknown();
                }
                Err(error.into_boundary())
            }
        }
    }

    /// Load a browser-produced File artifact for a later upload or host
    /// inspection. A reference that no longer exists is reported explicitly as
    /// `media_unavailable`; it is never synthesized from model input.
    pub fn load_artifact(
        &self,
        context: &ToolContext,
        reference: &str,
    ) -> Result<Vec<u8>, BoundaryError> {
        self.verify(context)?;
        let artifacts = self
            .artifacts
            .as_ref()
            .ok_or_else(|| failure("unsupported", "Browser file broker is not configured"))?;
        Ok(artifacts.load_artifact(reference)?.bytes)
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
        // Wake any in-flight provider call, notably `wait`, which holds the
        // page document lock. Cleanup itself uses the caller's cancellation,
        // not this lifecycle signal, so close/shutdown can still confirm.
        // The weak background binding is retained; a later explicit `open`
        // re-arms it unless the supervisor has already closed.
        self.lifecycle.send_replace(true);
        self.background_enabled.store(false, Ordering::Release);
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
                    // Provider shutdown below terminates the worker and is the
                    // ultimate cleanup even when the page close failed.
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
    #[serde(default)]
    background: bool,
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
    text: Option<String>,
    credential_slot: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DownloadArgs {
    page: String,
    document: String,
    element_ref: String,
    #[serde(default)]
    background: bool,
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
    #[serde(default)]
    background: bool,
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
            "Navigate one browser page to an HTTP(S) URL. Existing document and element references become stale. Set background=true to return a real job ID for a slow navigation.",
            serde_json::json!({"type":"object","properties":{"page":{"type":"string","minLength":1,"maxLength":64},"url":{"type":"string","minLength":1,"maxLength":8192},"background":{"type":"boolean"}},"required":["page","url"],"additionalProperties":false}),
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
            "Fill an element with bounded text or a host-registered origin-bound credential slot. Exactly one of text and credential_slot is required. Filling never submits the form.",
            serde_json::json!({"type":"object","properties":{"page":{"type":"string","minLength":1,"maxLength":64},"document":{"type":"string","minLength":1,"maxLength":64},"element_ref":{"type":"string","minLength":1,"maxLength":64},"text":{"type":"string","maxLength":65536},"credential_slot":{"type":"string","minLength":1,"maxLength":64}},"required":["page","document","element_ref"],"oneOf":[{"required":["text"],"not":{"required":["credential_slot"]}},{"required":["credential_slot"],"not":{"required":["text"]}}],"additionalProperties":false}),
        ),
        BrowserToolKind::Press => (
            "browser_press",
            "Press one bounded key on the page's current document token. A successful press invalidates the document.",
            serde_json::json!({"type":"object","properties":{"page":{"type":"string","minLength":1,"maxLength":64},"document":{"type":"string","minLength":1,"maxLength":64},"key":{"type":"string","minLength":1,"maxLength":128}},"required":["page","document","key"],"additionalProperties":false}),
        ),
        BrowserToolKind::Wait => (
            "browser_wait",
            "Wait a bounded time for the page. This does not confirm a document revision. Set background=true to return a real job ID.",
            serde_json::json!({"type":"object","properties":{"page":{"type":"string","minLength":1,"maxLength":64},"milliseconds":{"type":"integer","minimum":0,"maximum":settings.max_wait_ms},"background":{"type":"boolean"}},"required":["page","milliseconds"],"additionalProperties":false}),
        ),
        BrowserToolKind::Screenshot => (
            "browser_screenshot",
            "Capture a bounded screenshot with sensitive fields masked before capture. The host saves an Image artifact and returns source, mime, size and SHA-256 metadata; the image is offered to the model only when its profile enables images.",
            page_schema(),
        ),
        BrowserToolKind::Download => (
            "browser_download",
            "Click one element in the current document token and accept only the download produced by that action. The host stores a bounded, type-checked File artifact; set background=true to return a real job ID.",
            serde_json::json!({"type":"object","properties":{"page":{"type":"string","minLength":1,"maxLength":64},"document":{"type":"string","minLength":1,"maxLength":64},"element_ref":{"type":"string","minLength":1,"maxLength":64},"background":{"type":"boolean"}},"required":["page","document","element_ref"],"additionalProperties":false}),
        ),
        BrowserToolKind::Upload => (
            "browser_upload",
            "Upload one artifact reference owned by this Session to an element in the current document token. Host file paths are never accepted.",
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
    fn start_background(
        &self,
        parent_call_id: &str,
        context: &ToolContext,
        tool: &'static str,
        mutating: bool,
        page: String,
        operation: impl FnOnce(
            Arc<BrowserState>,
            ToolContext,
            Arc<dyn Cancellation>,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            (
                                serde_json::Value,
                                Vec<String>,
                                Vec<crate::output_store::SealedMedia>,
                            ),
                            BoundaryError,
                        >,
                    > + Send,
            >,
        > + Send
        + 'static,
    ) -> Result<CapturedToolResult, BoundaryError> {
        if !self.state.background_enabled.load(Ordering::Acquire) {
            return Err(failure(
                "unsupported",
                "Browser background actions require an open browser lifecycle",
            ));
        }
        let jobs = self
            .state
            .background
            .lock()
            .map_err(|_| failure("background_error", "Browser background lock failed"))?
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .filter(|jobs| jobs.is_closed().is_ok_and(|closed| !closed))
            .ok_or_else(|| {
                failure(
                    "unsupported",
                    "Browser background actions are not configured for this Run",
                )
            })?;
        let parent_call_id = parent_call_id.to_owned();
        let state = self.state.clone();
        let context_owned = context.clone();
        let job_id =
            jobs.start_browser(&parent_call_id, context, tool, mutating, move |cancel| {
                let state = state.clone();
                let context = context_owned.clone();
                Box::pin(async move {
                    let (value, media_refs, pins) = operation(state, context, cancel).await?;
                    Ok((
                        ToolResult {
                            ok: true,
                            data: Some(value),
                            error: None,
                            truncated: false,
                            media_refs,
                        },
                        pins,
                    ))
                })
            })?;
        Ok(CapturedToolResult {
            result: ToolResult {
                ok: true,
                data: Some(serde_json::json!({
                    "source": UNTRUSTED_BROWSER,
                    "status": "started",
                    "background": true,
                    "job_id": job_id,
                    "page": page,
                })),
                error: None,
                truncated: false,
                media_refs: Vec::new(),
            },
            outputs: Vec::new(),
            media: Vec::new(),
            capture_errors: Vec::new(),
        })
    }

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
        let mut media = Vec::new();
        let mut media_refs = Vec::new();
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
                if args.background {
                    let page = args.page.clone();
                    let url = args.url.clone();
                    return self.start_background(
                        &call.call_id,
                        context,
                        "browser_navigate",
                        true,
                        args.page.clone(),
                        move |state, context, cancel| {
                            Box::pin(async move {
                                let view = state
                                    .navigate(&context, &page, &url, cancel.as_ref())
                                    .await?;
                                Ok((
                                    serde_json::json!({
                                        "source": UNTRUSTED_BROWSER,
                                        "page": view.page,
                                        "url": view.url,
                                        "document_stale": view.document_stale,
                                    }),
                                    Vec::new(),
                                    Vec::new(),
                                ))
                            })
                        },
                    );
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
                        media_refs: Vec::new(),
                    },
                    outputs: sealed,
                    media: Vec::new(),
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
                {
                    return Err(failure("invalid_args", "invalid browser fill arguments"));
                }
                let view = match (&args.text, &args.credential_slot) {
                    (Some(text), None) => {
                        if text.len() > MAX_FILL_BYTES || text.contains('\0') {
                            return Err(failure("invalid_args", "invalid browser fill arguments"));
                        }
                        self.state
                            .fill(
                                context,
                                &args.page,
                                &args.document,
                                &args.element_ref,
                                text,
                                cancel,
                            )
                            .await?
                    }
                    (None, Some(slot)) => {
                        if !crate::browser::settings::valid_alias(slot) {
                            return Err(failure("invalid_args", "invalid browser credential slot"));
                        }
                        self.state
                            .fill_credential(
                                context,
                                &args.page,
                                &args.document,
                                &args.element_ref,
                                slot,
                                cancel,
                            )
                            .await?
                    }
                    _ => {
                        return Err(failure(
                            "invalid_args",
                            "browser fill requires exactly one of text or credential_slot",
                        ));
                    }
                };
                serde_json::json!({"source":UNTRUSTED_BROWSER,"page":view.page,"url":view.url,"detail":view.detail,"document_stale":view.document_stale,"document_revision":view.document_revision,"credential_slot":view.credential_slot,"credential_uses_remaining":view.credential_uses_remaining})
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
                if args.background {
                    let page = args.page.clone();
                    let milliseconds = args.milliseconds;
                    return self.start_background(
                        &call.call_id,
                        context,
                        "browser_wait",
                        false,
                        args.page.clone(),
                        move |state, context, cancel| {
                            Box::pin(async move {
                                let view = state
                                    .wait(&context, &page, milliseconds, cancel.as_ref())
                                    .await?;
                                Ok((
                                    serde_json::json!({
                                        "source": UNTRUSTED_BROWSER,
                                        "page": view.page,
                                        "url": view.url,
                                        "detail": view.detail,
                                        "document_stale": view.document_stale,
                                        "document_revision": view.document_revision,
                                    }),
                                    Vec::new(),
                                    Vec::new(),
                                ))
                            })
                        },
                    );
                }
                let view = self
                    .state
                    .wait(context, &args.page, args.milliseconds, cancel)
                    .await?;
                serde_json::json!({"source":UNTRUSTED_BROWSER,"page":view.page,"url":view.url,"detail":view.detail,"document_stale":view.document_stale,"document_revision":view.document_revision})
            }
            BrowserToolKind::Screenshot => {
                let page = parse_page(call)?;
                let (view, sealed) = self.state.screenshot(context, &page, cancel).await?;
                media_refs = view.media_ref.clone().into_iter().collect();
                media.extend(sealed);
                let encoded = serde_json::to_value(&view).map_err(|_| {
                    failure(
                        "serialization_error",
                        "Browser screenshot cannot be encoded",
                    )
                })?;
                serde_json::json!({
                    "source":UNTRUSTED_BROWSER,
                    "screenshot":encoded,
                })
            }
            BrowserToolKind::Download => {
                let args: DownloadArgs = serde_json::from_str(&call.arguments)
                    .map_err(|_| failure("invalid_args", "invalid browser download arguments"))?;
                if !valid_token(&args.page, "page_")
                    || !valid_token(&args.document, "doc_")
                    || !valid_token(&args.element_ref, "el_")
                {
                    return Err(failure(
                        "invalid_args",
                        "invalid browser download arguments",
                    ));
                }
                if args.background {
                    let page = args.page.clone();
                    let document = args.document.clone();
                    let element = args.element_ref.clone();
                    return self.start_background(
                        &call.call_id,
                        context,
                        "browser_download",
                        true,
                        args.page.clone(),
                        move |state, context, cancel| {
                            Box::pin(async move {
                                let (view, sealed) = state
                                    .download(&context, &page, &document, &element, cancel.as_ref())
                                    .await?;
                                let media_refs =
                                    view.media_ref.clone().into_iter().collect::<Vec<_>>();
                                // The supervisor holds the artifact lease in
                                // its job state until the Run ends, so GC
                                // cannot reclaim it before status/upload.
                                Ok((
                                    serde_json::json!({
                                        "source": UNTRUSTED_BROWSER,
                                        "download": view,
                                    }),
                                    media_refs,
                                    sealed.into_iter().collect(),
                                ))
                            })
                        },
                    );
                }
                let (view, sealed) = self
                    .state
                    .download(
                        context,
                        &args.page,
                        &args.document,
                        &args.element_ref,
                        cancel,
                    )
                    .await?;
                media_refs = view.media_ref.clone().into_iter().collect();
                media.extend(sealed);
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
                media_refs.push(view.artifact_ref.clone());
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
                media_refs,
            },
            outputs: Vec::new(),
            media,
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
        (state.screenshot_available(), BrowserToolKind::Screenshot),
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
#[cfg(test)]
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
    screenshot_unmasked: AtomicBool,
    override_screenshot_url: Mutex<Option<String>>,
    override_action_url: Mutex<Option<String>>,
    omit_navigation_url: AtomicBool,
    override_title: Mutex<Option<String>>,
    override_download: Mutex<Option<BrowserDownload>>,
    download_failure: Mutex<Option<(String, bool)>>,
    last_upload: Mutex<Option<BrowserUploadSource>>,
    last_fill: Mutex<Option<String>>,
    fill_count: AtomicU64,
    shutdown_delay_ms: u64,
    next_element: AtomicU64,
    framed_element: AtomicBool,
    pause_started: Arc<tokio::sync::Notify>,
}
#[cfg(test)]
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
            screenshot_unmasked: AtomicBool::new(false),
            override_screenshot_url: Mutex::new(None),
            override_action_url: Mutex::new(None),
            omit_navigation_url: AtomicBool::new(false),
            override_title: Mutex::new(None),
            override_download: Mutex::new(None),
            download_failure: Mutex::new(None),
            last_upload: Mutex::new(None),
            last_fill: Mutex::new(None),
            fill_count: AtomicU64::new(0),
            shutdown_delay_ms: 0,
            next_element: AtomicU64::new(1),
            framed_element: AtomicBool::new(false),
            pause_started: Arc::new(tokio::sync::Notify::new()),
        }
    }
    pub fn full() -> Self {
        Self::new(BrowserFacts {
            provider: "fixture".into(),
            screenshots: true,
            screenshot_masking: true,
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
    /// Report the fixture element as belonging to a child frame.
    pub fn with_framed_element(mut self) -> Self {
        self.framed_element = AtomicBool::new(true);
        self
    }
    /// Simulate a provider that captures without masking sensitive fields.
    pub fn with_unmasked_screenshot(mut self) -> Self {
        self.screenshot_unmasked = AtomicBool::new(true);
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
    /// Make the next action report this provider-side final URL.
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
    /// Make the next download action return this exact provider payload.
    pub fn return_next_download(&self, download: BrowserDownload) {
        if let Ok(mut value) = self.override_download.lock() {
            *value = Some(download);
        }
    }
    /// Make the next download call fail with a known or unknown provider error.
    pub fn fail_next_download(&self, code: &str, uncertain: bool) {
        if let Ok(mut value) = self.download_failure.lock() {
            *value = Some((code.into(), uncertain));
        }
    }
    fn take_download_failure(&self) -> Option<(String, bool)> {
        self.download_failure
            .lock()
            .ok()
            .and_then(|mut value| value.take())
    }
    pub fn fill_count(&self) -> u64 {
        self.fill_count.load(Ordering::Acquire)
    }
    pub fn last_fill(&self) -> Option<String> {
        self.last_fill.lock().ok().and_then(|value| value.clone())
    }
    pub fn last_upload(&self) -> Option<BrowserUploadSource> {
        self.last_upload.lock().ok().and_then(|value| value.clone())
    }
    fn take_override_download(&self) -> Option<BrowserDownload> {
        self.override_download
            .lock()
            .ok()
            .and_then(|mut value| value.take())
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
        self.pause_started.notify_one();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(BrowserFailure::new(
                "cancelled",
                "fixture provider observed cancellation",
            )),
            _ = tokio::time::sleep(std::time::Duration::from_millis(milliseconds)) => Ok(()),
        }
    }
    pub fn pause_started(&self) -> Arc<tokio::sync::Notify> {
        self.pause_started.clone()
    }
}

#[async_trait::async_trait]
#[cfg(test)]
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
            url: (!self.omit_navigation_url.swap(false, Ordering::AcqRel)).then_some(final_url),
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
                    frame: self
                        .framed_element
                        .load(Ordering::Acquire)
                        .then(|| "f1".to_owned()),
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
            url: self.take_override_action_url(),
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
        if self.take_unknown() {
            return Err(BrowserFailure::uncertain(
                "fixture fill may have been dispatched",
            ));
        }
        self.fill_count.fetch_add(1, Ordering::AcqRel);
        if let Ok(mut value) = self.last_fill.lock() {
            *value = Some(text.to_owned());
        }
        Ok(BrowserAction {
            url: self.take_override_action_url(),
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
            url: self.take_override_action_url(),
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
            masked: !self.screenshot_unmasked.load(Ordering::Acquire),
            url: self
                .override_screenshot_url
                .lock()
                .ok()
                .and_then(|mut value| value.take()),
        })
    }
    async fn download(
        &self,
        _session: &str,
        _page: &str,
        _element: &str,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserDownload, BrowserFailure> {
        Self::cancelled(cancel)?;
        if self.take_stall_action() {
            std::future::pending::<()>().await;
        }
        if let Some((code, uncertain)) = self.take_download_failure() {
            return Err(if uncertain {
                BrowserFailure::uncertain("fixture download outcome is unknown")
            } else {
                BrowserFailure::new(code, "fixture download failed")
            });
        }
        Ok(self.take_override_download().unwrap_or(BrowserDownload {
            filename: "download.txt".into(),
            mime: "text/plain".into(),
            bytes: b"fixture download".to_vec(),
        }))
    }
    async fn upload(
        &self,
        _session: &str,
        _page: &str,
        _element: &str,
        source: &BrowserUploadSource,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserUpload, BrowserFailure> {
        Self::cancelled(cancel)?;
        if self.take_stall_action() {
            std::future::pending::<()>().await;
        }
        if let Ok(mut value) = self.last_upload.lock() {
            *value = Some(source.clone());
        }
        Ok(BrowserUpload {
            filename: source.filename.clone(),
            bytes_sent: source.size,
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
    use crate::{
        contract::{PermissionMode, RunId, WorkMode},
        credentials::{
            CredentialScope, CredentialValue, FileCredentialStore, SecretRegistry, SecretValue,
        },
        network::NormalizedOrigin,
    };
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

    fn allowed_settings(origin: &str) -> BrowserSettings {
        BrowserSettings {
            enabled: true,
            network: crate::browser::network::BrowserNetworkSettings {
                enabled: true,
                allowed_origins: vec![origin.into(), "http://other.example".into()],
                localhost_development_origin: None,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    async fn credential_state(
        root: &std::path::Path,
        origin: &str,
        max_uses: u32,
    ) -> (Arc<FixtureProvider>, BrowserState, Run, FileCredentialStore) {
        let registry = Arc::new(SecretRegistry::default());
        let store = FileCredentialStore::open(root.join("credentials"), registry.clone()).unwrap();
        let scope =
            CredentialScope::for_browser("login", &NormalizedOrigin::parse(origin).unwrap())
                .unwrap();
        store
            .create(
                scope,
                CredentialValue::Secret(SecretValue::new("super-secret-value").unwrap()),
            )
            .await
            .unwrap();
        let settings = BrowserSettings {
            credentials: BrowserCredentialSettings {
                enabled: true,
                store: Some(root.join("credentials")),
                slots: vec![BrowserCredentialSlot {
                    alias: "login".into(),
                    credential_ref: store.list().await.unwrap().first().unwrap().reference,
                    origin: origin.into(),
                    purpose: BrowserCredentialPurpose::FormFill,
                    max_uses,
                }],
            },
            ..allowed_settings(origin)
        };
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let broker = BrowserCredentialBroker::new(&settings.credentials, store.clone()).unwrap();
        let state = BrowserState::new(settings, provider.clone(), run.clone())
            .unwrap()
            .with_credentials(broker);
        (provider, state, run, store)
    }

    async fn artifact_state(
        root: &std::path::Path,
        provider: Arc<FixtureProvider>,
        run: &Run,
    ) -> (BrowserState, Arc<OutputStore>) {
        let outputs = Arc::new(
            OutputStore::open(
                root.join("outputs"),
                crate::output_store::OutputLimits::default(),
                Arc::new(SecretRegistry::default()),
            )
            .unwrap(),
        );
        let state = make_state(provider, run)
            .with_artifacts(outputs.clone())
            .unwrap();
        (state, outputs)
    }

    fn stored_media_files(outputs: &Arc<OutputStore>) -> usize {
        std::fs::read_dir(outputs.protected_path())
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        entry
                            .file_name()
                            .to_str()
                            .is_some_and(|name| name.ends_with(".media"))
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    async fn open_document(
        state: &BrowserState,
        context: &ToolContext,
        url: &str,
    ) -> (String, SnapshotView) {
        let opened = state.open(context, &NeverCancel).await.unwrap();
        state
            .navigate(context, &opened.page, url, &NeverCancel)
            .await
            .unwrap();
        let snapshot = state
            .snapshot(context, &opened.page, &NeverCancel)
            .await
            .unwrap();
        (opened.page, snapshot)
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
    async fn navigate_rejects_targets_outside_the_network_policy_before_dispatch() {
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = BrowserState::new(
            BrowserSettings {
                enabled: true,
                network: crate::browser::network::BrowserNetworkSettings {
                    enabled: true,
                    allowed_origins: vec!["http://127.0.0.1:8080".into()],
                    localhost_development_origin: Some("http://127.0.0.1:8080".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
            provider,
            run.clone(),
        )
        .unwrap();
        let context = make_context(run);
        let cancel = NeverCancel;
        let opened = state.open(&context, &cancel).await.unwrap();
        let denied = state
            .navigate(&context, &opened.page, "http://169.254.169.254/", &cancel)
            .await
            .unwrap_err();
        assert_eq!(denied.0.code, "browser_network_denied");
        let allowed = state
            .navigate(&context, &opened.page, "http://127.0.0.1:8080/", &cancel)
            .await
            .unwrap();
        assert_eq!(allowed.url, "http://127.0.0.1:8080/");
    }

    #[tokio::test]
    async fn navigate_accepts_fragments_and_oauth_style_redirects_from_allowed_origins() {
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = BrowserState::new(
            BrowserSettings {
                enabled: true,
                network: crate::browser::network::BrowserNetworkSettings {
                    enabled: true,
                    allowed_origins: vec!["http://127.0.0.1:8080".into()],
                    localhost_development_origin: Some("http://127.0.0.1:8080".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
            provider.clone(),
            run.clone(),
        )
        .unwrap();
        let context = make_context(run);
        let cancel = NeverCancel;
        let opened = state.open(&context, &cancel).await.unwrap();
        let final_url = "http://127.0.0.1:8080/callback?code=one#access_token=secret";
        provider.return_next_action_url(final_url);
        let view = state
            .navigate(
                &context,
                &opened.page,
                "http://127.0.0.1:8080/login",
                &cancel,
            )
            .await
            .unwrap();
        // The fragment is preserved for provenance but never becomes part of
        // the origin checked against the allowlist.
        assert_eq!(view.url, final_url);

        provider.return_next_action_url("http://127.0.0.1:8080/#");
        assert!(
            state
                .navigate(&context, &opened.page, "http://127.0.0.1:8080/", &cancel)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn navigate_treats_about_blank_and_browser_error_pages_as_explicit_states() {
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = make_state(provider.clone(), &run);
        let context = make_context(run);
        let cancel = NeverCancel;
        let opened = state.open(&context, &cancel).await.unwrap();
        for final_url in [
            "about:blank",
            "about:blank?from=open",
            "about:blank#section",
            "about:blank?from=open#section",
            "ABOUT:blank",
            "chrome-error://chromewebdata/",
            "chrome-error://chromewebdata/#ignored",
        ] {
            provider.return_next_action_url(final_url);
            let view = state
                .navigate(&context, &opened.page, "https://example.invalid/", &cancel)
                .await
                .unwrap();
            assert_eq!(view.url, final_url);
        }
        // A non-blank `about:` document and unknown schemes stay invalid
        // provider results rather than being silently accepted.
        for final_url in ["about:config", "about:blank/", "data:text/html,x"] {
            provider.return_next_action_url(final_url);
            let error = state
                .navigate(&context, &opened.page, "https://example.invalid/", &cancel)
                .await
                .unwrap_err();
            assert_eq!(error.0.code, "invalid_provider_result", "{final_url}");
        }
    }

    #[tokio::test]
    async fn navigate_rejects_credentialed_or_non_http_final_urls() {
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = make_state(provider.clone(), &run);
        let context = make_context(run);
        let cancel = NeverCancel;
        let opened = state.open(&context, &cancel).await.unwrap();
        for final_url in [
            "http://user:pass@example.invalid/",
            "http://user@example.invalid/",
            "file:///etc/passwd",
            "data:text/html,secret",
        ] {
            provider.return_next_action_url(final_url);
            let error = state
                .navigate(&context, &opened.page, "https://example.invalid/", &cancel)
                .await
                .unwrap_err();
            assert_eq!(
                error.0.code, "invalid_provider_result",
                "final URL {final_url}"
            );
            // The provider reported no origin that could be authorized and the
            // error message never echoes the URL or credentials.
            assert!(!error.0.message.contains("pass"));
            assert!(!error.0.message.contains("secret"));
        }
    }

    #[tokio::test]
    async fn native_path_without_network_does_not_cross_check_final_origin() {
        // Default settings have `network.enabled = false`; the final URL is
        // still syntactically validated but its origin is not compared against
        // an unrelated policy that this backend never uses.
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = make_state(provider.clone(), &run);
        let context = make_context(run);
        let cancel = NeverCancel;
        let opened = state.open(&context, &cancel).await.unwrap();
        provider.return_next_action_url("https://outside.example/path#fragment");
        let view = state
            .navigate(&context, &opened.page, "https://allowed.example/", &cancel)
            .await
            .unwrap();
        assert_eq!(view.url, "https://outside.example/path#fragment");
    }

    #[tokio::test]
    async fn navigate_still_denies_a_disallowed_final_origin_when_network_is_enabled() {
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = BrowserState::new(
            BrowserSettings {
                enabled: true,
                network: crate::browser::network::BrowserNetworkSettings {
                    enabled: true,
                    allowed_origins: vec!["http://127.0.0.1:8080".into()],
                    localhost_development_origin: Some("http://127.0.0.1:8080".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
            provider.clone(),
            run.clone(),
        )
        .unwrap();
        let context = make_context(run);
        let cancel = NeverCancel;
        let opened = state.open(&context, &cancel).await.unwrap();
        provider.return_next_action_url("https://outside.example/path");
        let error = state
            .navigate(&context, &opened.page, "http://127.0.0.1:8080/", &cancel)
            .await
            .unwrap_err();
        assert_eq!(error.0.code, "network_final_origin_denied");
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

        let pause_started = provider.pause_started();
        let flag = Arc::new(AtomicBool::new(false));
        let cancel = SwitchCancel(flag.clone());
        let trigger = flag.clone();
        let task = tokio::spawn(async move {
            pause_started.notified().await;
            trigger.store(true, Ordering::Release);
        });
        let error = state
            .wait(&context, &opened.page, 5_000, &cancel)
            .await
            .unwrap_err();
        task.await.unwrap();
        assert_eq!(error.0.code, "cancelled");

        let provider = Arc::new(FixtureProvider::full().with_delay(5_000));
        let pause_started = provider.pause_started();
        let run = make_run();
        let state = make_state(provider, &run);
        let context = make_context(run);
        let opened = state.open(&context, &NeverCancel).await.unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let cancel = SwitchCancel(flag.clone());
        let trigger = flag.clone();
        let task = tokio::spawn(async move {
            pause_started.notified().await;
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
            let root = tempfile::tempdir().unwrap();
            let (state, outputs) = if matches!(action, Action::Upload | Action::Download) {
                let (state, outputs) = artifact_state(root.path(), provider.clone(), &run).await;
                (state, Some(outputs))
            } else {
                (make_state(provider.clone(), &run), None)
            };
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
                Action::Upload => {
                    let artifact = outputs
                        .as_ref()
                        .unwrap()
                        .put_file(
                            context.run.session_id,
                            "text/plain",
                            "upload.txt",
                            b"upload bytes",
                        )
                        .unwrap();
                    state
                        .upload(
                            &context,
                            &opened.page,
                            &document,
                            &element,
                            &artifact.reference,
                            &cancel,
                        )
                        .await
                        .unwrap_err()
                }
                Action::Download => state
                    .download(&context, &opened.page, &document, &element, &cancel)
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

    #[test]
    fn settings_reject_selecting_two_worker_backends() {
        let mut settings = BrowserSettings::default();
        settings.worker.enabled = true;
        settings.worker.node = Some("/opt/node".into());
        settings.worker.worker_root = Some("/opt/worker".into());
        settings.worker.chromium = Some("/opt/chromium".into());
        settings.worker.identity_manifest = Some("/opt/manifest.json".into());
        settings.worker.identity_sha256 = Some("0".repeat(64));
        settings.worker.validate().unwrap();
        settings.vm.enabled = true;
        settings.vm.colima = Some("/usr/local/bin/colima".into());
        settings.vm.ssh = Some("/usr/bin/ssh".into());
        settings.vm.profile = Some("saya-browser".into());
        settings.vm.host = Some("127.0.0.1".into());
        settings.vm.user = Some("saya".into());
        settings.vm.port = Some(2222);
        settings.vm.identity_file = Some("/tmp/key".into());
        settings.vm.known_hosts = Some("/tmp/known_hosts".into());
        settings.vm.node = Some("/opt/node".into());
        settings.vm.worker_root = Some("/opt/worker".into());
        settings.vm.chromium =
            Some("/opt/browsers/chromium_headless_shell-1254/chrome-headless-shell".into());
        settings.vm.identity_manifest = Some("/opt/manifest.json".into());
        settings.vm.identity_sha256 = Some("0".repeat(64));
        settings.vm.remote_temp_root = Some("/tmp/runs".into());
        assert_eq!(settings.validate().unwrap_err().0.code, "invalid_config");
    }

    #[tokio::test]
    async fn credential_fill_injects_only_at_the_final_boundary_and_never_leaks_plaintext() {
        let root = tempfile::tempdir().unwrap();
        let origin = "https://allowed.example";
        let (provider, state, run, _store) = credential_state(root.path(), origin, 3).await;
        let context = make_context(run);
        let (page, snapshot) = open_document(&state, &context, &format!("{origin}/login")).await;
        let view = state
            .fill_credential(
                &context,
                &page,
                &snapshot.document,
                &snapshot.elements[0].element_ref,
                "login",
                &NeverCancel,
            )
            .await
            .unwrap();
        assert_eq!(provider.fill_count(), 1);
        assert_eq!(provider.last_fill().as_deref(), Some("super-secret-value"));
        assert_eq!(view.credential_slot.as_deref(), Some("login"));
        assert_eq!(view.credential_uses_remaining, Some(2));
        let encoded = serde_json::to_string(&view).unwrap();
        assert!(!encoded.contains("super-secret-value"));
        assert!(!encoded.contains("super-secret"));
    }

    #[tokio::test]
    async fn credential_fill_rejects_wrong_origin_stale_document_and_exhausted_uses() {
        let root = tempfile::tempdir().unwrap();
        let origin = "https://allowed.example";
        let (provider, state, run, _store) = credential_state(root.path(), origin, 1).await;
        let context = make_context(run);
        let (page, snapshot) = open_document(&state, &context, &format!("{origin}/login")).await;

        // A stale document token is rejected before the slot is consumed.
        let fresh = state.snapshot(&context, &page, &NeverCancel).await.unwrap();
        assert_eq!(
            state
                .fill_credential(
                    &context,
                    &page,
                    &snapshot.document,
                    &fresh.elements[0].element_ref,
                    "login",
                    &NeverCancel,
                )
                .await
                .unwrap_err()
                .0
                .code,
            "stale_element"
        );
        assert_eq!(provider.fill_count(), 0);

        // A different page origin cannot use the slot.
        provider.return_next_action_url("http://other.example/");
        state
            .navigate(&context, &page, "http://other.example/", &NeverCancel)
            .await
            .unwrap();
        let other = state.snapshot(&context, &page, &NeverCancel).await.unwrap();
        assert_eq!(
            state
                .fill_credential(
                    &context,
                    &page,
                    &other.document,
                    &other.elements[0].element_ref,
                    "login",
                    &NeverCancel,
                )
                .await
                .unwrap_err()
                .0
                .code,
            "credential_origin_mismatch"
        );
        assert_eq!(provider.fill_count(), 0);

        // One successful use exhausts max_uses=1.
        provider.return_next_action_url(format!("{origin}/login"));
        state
            .navigate(&context, &page, &format!("{origin}/login"), &NeverCancel)
            .await
            .unwrap();
        let again = state.snapshot(&context, &page, &NeverCancel).await.unwrap();
        state
            .fill_credential(
                &context,
                &page,
                &again.document,
                &again.elements[0].element_ref,
                "login",
                &NeverCancel,
            )
            .await
            .unwrap();
        let after = state.snapshot(&context, &page, &NeverCancel).await.unwrap();
        assert_eq!(
            state
                .fill_credential(
                    &context,
                    &page,
                    &after.document,
                    &after.elements[0].element_ref,
                    "login",
                    &NeverCancel,
                )
                .await
                .unwrap_err()
                .0
                .code,
            "credential_uses_exhausted"
        );
        assert_eq!(provider.fill_count(), 1);
    }

    #[tokio::test]
    async fn unknown_fill_consumes_a_use_conservatively() {
        let root = tempfile::tempdir().unwrap();
        let origin = "https://allowed.example";
        let (provider, state, run, _store) = credential_state(root.path(), origin, 2).await;
        let context = make_context(run);
        let (page, snapshot) = open_document(&state, &context, &format!("{origin}/login")).await;
        provider.fail_next_unknown();
        assert_eq!(
            state
                .fill_credential(
                    &context,
                    &page,
                    &snapshot.document,
                    &snapshot.elements[0].element_ref,
                    "login",
                    &NeverCancel,
                )
                .await
                .unwrap_err()
                .0
                .code,
            "outcome_unknown"
        );
        let recovered = state.snapshot(&context, &page, &NeverCancel).await.unwrap();
        let view = state
            .fill_credential(
                &context,
                &page,
                &recovered.document,
                &recovered.elements[0].element_ref,
                "login",
                &NeverCancel,
            )
            .await
            .unwrap();
        assert_eq!(view.credential_uses_remaining, Some(0));
    }

    #[tokio::test]
    async fn fill_adapter_requires_exactly_one_input_and_hides_credential_values() {
        let root = tempfile::tempdir().unwrap();
        let origin = "https://allowed.example";
        let (provider, state, run, _store) = credential_state(root.path(), origin, 2).await;
        let context = make_context(run);
        let (page, snapshot) = open_document(&state, &context, &format!("{origin}/login")).await;
        let state = Arc::new(state);
        let tool = BrowserTool {
            state: state.clone(),
            kind: BrowserToolKind::Fill,
        };
        let base = serde_json::json!({
            "page": page,
            "document": snapshot.document,
            "element_ref": snapshot.elements[0].element_ref,
        });
        let schema = tool.definition().input_schema;
        assert_eq!(schema["oneOf"].as_array().unwrap().len(), 2);

        let mut both = base.clone();
        both["text"] = serde_json::json!("plain");
        both["credential_slot"] = serde_json::json!("login");
        let both = ToolCall {
            call_id: "fill-both".into(),
            name: "browser_fill".into(),
            arguments: both.to_string(),
        };
        assert_eq!(
            tool.execute_captured(&both, &context, &NeverCancel, None)
                .await
                .unwrap_err()
                .0
                .code,
            "invalid_args"
        );
        let neither = ToolCall {
            call_id: "fill-neither".into(),
            name: "browser_fill".into(),
            arguments: base.to_string(),
        };
        assert_eq!(
            tool.execute_captured(&neither, &context, &NeverCancel, None)
                .await
                .unwrap_err()
                .0
                .code,
            "invalid_args"
        );
        let mut slot = base;
        slot["credential_slot"] = serde_json::json!("login");
        let slot = ToolCall {
            call_id: "fill-slot".into(),
            name: "browser_fill".into(),
            arguments: slot.to_string(),
        };
        let result = tool
            .execute_captured(&slot, &context, &NeverCancel, None)
            .await
            .unwrap();
        assert_eq!(provider.fill_count(), 1);
        let data = result.result.data.unwrap();
        assert_eq!(data["credential_slot"], "login");
        assert!(!data.to_string().contains("super-secret-value"));
        assert!(result.media.is_empty());
    }

    #[tokio::test]
    async fn download_stores_a_type_checked_file_artifact_and_sanitizes_names() {
        let root = tempfile::tempdir().unwrap();
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let (state, _outputs) = artifact_state(root.path(), provider.clone(), &run).await;
        let context = make_context(run);
        let (page, snapshot) = open_document(&state, &context, "http://allowed.example/dl").await;
        provider.return_next_download(BrowserDownload {
            filename: "../../escape.txt".into(),
            mime: "text/plain".into(),
            bytes: b"downloaded bytes".to_vec(),
        });
        let (view, _sealed) = state
            .download(
                &context,
                &page,
                &snapshot.document,
                &snapshot.elements[0].element_ref,
                &NeverCancel,
            )
            .await
            .unwrap();
        assert_eq!(view.filename, "escape.txt");
        assert_eq!(view.mime, "text/plain");
        assert!(view.artifact_stored);
        assert_eq!(view.bytes, 16);
        let reference = view.artifact_ref.clone().unwrap();
        assert_eq!(
            state.load_artifact(&context, &reference).unwrap(),
            b"downloaded bytes"
        );
        assert_eq!(view.artifact_ref, view.media_ref);
        // The host-only durable reference is never part of the interface data.
        let encoded = serde_json::to_value(&view).unwrap();
        assert!(encoded.get("media_ref").is_none());
        assert!(encoded["artifact_ref"].as_str().is_some());
    }

    #[tokio::test]
    async fn download_rejects_oversize_and_executable_payloads() {
        let root = tempfile::tempdir().unwrap();
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let mut settings = BrowserSettings {
            enabled: true,
            ..Default::default()
        };
        settings.files.max_download_bytes = 8;
        let outputs = Arc::new(
            OutputStore::open(
                root.path().join("outputs"),
                crate::output_store::OutputLimits::default(),
                Arc::new(SecretRegistry::default()),
            )
            .unwrap(),
        );
        let state = BrowserState::new(settings, provider.clone(), run.clone())
            .unwrap()
            .with_artifacts(outputs)
            .unwrap();
        let context = make_context(run);
        let (page, snapshot) = open_document(&state, &context, "http://allowed.example/dl").await;
        provider.return_next_download(BrowserDownload {
            filename: "large.txt".into(),
            mime: "text/plain".into(),
            bytes: vec![b'x'; 9],
        });
        assert_eq!(
            state
                .download(
                    &context,
                    &page,
                    &snapshot.document,
                    &snapshot.elements[0].element_ref,
                    &NeverCancel,
                )
                .await
                .unwrap_err()
                .0
                .code,
            "file_too_large"
        );
        let snapshot = state.snapshot(&context, &page, &NeverCancel).await.unwrap();
        provider.return_next_download(BrowserDownload {
            filename: "malware.bin".into(),
            mime: "application/octet-stream".into(),
            bytes: b"MZ\x90\x00".to_vec(),
        });
        assert_eq!(
            state
                .download(
                    &context,
                    &page,
                    &snapshot.document,
                    &snapshot.elements[0].element_ref,
                    &NeverCancel,
                )
                .await
                .unwrap_err()
                .0
                .code,
            "file_type_denied"
        );
    }

    #[tokio::test]
    async fn upload_accepts_only_session_artifacts_and_cleans_private_staging() {
        let root = tempfile::tempdir().unwrap();
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let (state, outputs) = artifact_state(root.path(), provider.clone(), &run).await;
        let context = make_context(run);
        let (page, snapshot) = open_document(&state, &context, "http://allowed.example/form").await;

        // Arbitrary host paths and foreign-session artifacts are rejected.
        for reference in [
            "/etc/passwd",
            "../artifact",
            "media_00000000000000000000000000000000_0000000000000000000000000000000000000000000000000000000000000000",
        ] {
            assert!(
                state
                    .upload(
                        &context,
                        &page,
                        &snapshot.document,
                        &snapshot.elements[0].element_ref,
                        reference,
                        &NeverCancel,
                    )
                    .await
                    .is_err(),
                "reference {reference} must be rejected"
            );
        }
        let other = outputs
            .put_file(
                SessionId::new(),
                "text/plain",
                "other.txt",
                b"other session",
            )
            .unwrap();
        assert_eq!(
            state
                .upload(
                    &context,
                    &page,
                    &snapshot.document,
                    &snapshot.elements[0].element_ref,
                    &other.reference,
                    &NeverCancel,
                )
                .await
                .unwrap_err()
                .0
                .code,
            "media_denied"
        );

        let artifact = outputs
            .put_file(
                context.run.session_id,
                "text/plain",
                "note.txt",
                b"upload bytes",
            )
            .unwrap();
        let snapshot = state.snapshot(&context, &page, &NeverCancel).await.unwrap();
        let view = state
            .upload(
                &context,
                &page,
                &snapshot.document,
                &snapshot.elements[0].element_ref,
                &artifact.reference,
                &NeverCancel,
            )
            .await
            .unwrap();
        assert_eq!(view.filename, "note.txt");
        assert_eq!(view.bytes_sent, 12);
        assert_eq!(view.artifact_ref, artifact.reference);
        let staged = provider.last_upload().unwrap();
        assert_eq!(staged.size, 12);
        assert!(!staged.path.exists(), "staging file must be removed");
    }

    #[tokio::test]
    async fn cancelled_download_and_upload_report_unknown_without_replay() {
        for download in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let provider = Arc::new(FixtureProvider::full());
            let run = make_run();
            let (state, outputs) = artifact_state(root.path(), provider.clone(), &run).await;
            let context = make_context(run);
            let (page, snapshot) =
                open_document(&state, &context, "http://allowed.example/file").await;
            provider.stall_next_action();
            let flag = Arc::new(AtomicBool::new(false));
            let cancel = SwitchCancel(flag.clone());
            let trigger = flag.clone();
            let task = tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                trigger.store(true, Ordering::Release);
            });
            let error = if download {
                state
                    .download(
                        &context,
                        &page,
                        &snapshot.document,
                        &snapshot.elements[0].element_ref,
                        &cancel,
                    )
                    .await
                    .unwrap_err()
            } else {
                let artifact = outputs
                    .put_file(
                        context.run.session_id,
                        "text/plain",
                        "note.txt",
                        b"upload bytes",
                    )
                    .unwrap();
                state
                    .upload(
                        &context,
                        &page,
                        &snapshot.document,
                        &snapshot.elements[0].element_ref,
                        &artifact.reference,
                        &cancel,
                    )
                    .await
                    .unwrap_err()
            };
            task.await.unwrap();
            assert_eq!(error.0.code, "outcome_unknown");
            // A fresh snapshot is the only recovery point; the dispatched
            // action itself is never replayed.
            assert!(state.snapshot(&context, &page, &NeverCancel).await.is_ok());
        }
    }

    #[tokio::test]
    async fn revoked_credential_fill_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let origin = "https://allowed.example";
        let (provider, state, run, store) = credential_state(root.path(), origin, 2).await;
        let context = make_context(run);
        let (page, snapshot) = open_document(&state, &context, &format!("{origin}/login")).await;
        let reference = store.list().await.unwrap()[0].reference;
        store
            .revoke(
                reference,
                &CredentialScope::for_browser("login", &NormalizedOrigin::parse(origin).unwrap())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            state
                .fill_credential(
                    &context,
                    &page,
                    &snapshot.document,
                    &snapshot.elements[0].element_ref,
                    "login",
                    &NeverCancel,
                )
                .await
                .unwrap_err()
                .0
                .code,
            "credential_revoked"
        );
        assert_eq!(provider.fill_count(), 0);
    }

    #[tokio::test]
    async fn screenshot_url_is_last_confirmed_and_actions_replace_or_clear_it() {
        let root = tempfile::tempdir().unwrap();
        let origin = "https://allowed.example";
        let (provider, state, run, _) = credential_state(root.path(), origin, 32).await;
        let (_, outputs) = artifact_state(root.path(), provider.clone(), &run).await;
        let state = state.with_artifacts(outputs).unwrap();
        let context = make_context(run);
        let opened = state.open(&context, &NeverCancel).await.unwrap();
        let (shot, _) = state
            .screenshot(&context, &opened.page, &NeverCancel)
            .await
            .unwrap();
        assert_eq!(shot.url.as_deref(), Some("about:blank"));
        assert_eq!(shot.url_observation, "last_confirmed");
        for action in ["click", "fill", "credential", "press", "wait"] {
            for final_url in [
                Some("http://other.example/next"),
                None,
                Some("file:///secret"),
            ] {
                state
                    .navigate(&context, &opened.page, origin, &NeverCancel)
                    .await
                    .unwrap();
                let snapshot = state
                    .snapshot(&context, &opened.page, &NeverCancel)
                    .await
                    .unwrap();
                if let Some(url) = final_url {
                    provider.return_next_action_url(url);
                }
                let element = &snapshot.elements[0].element_ref;
                let result = match action {
                    "click" => {
                        state
                            .click(
                                &context,
                                &opened.page,
                                &snapshot.document,
                                element,
                                None,
                                &NeverCancel,
                            )
                            .await
                    }
                    "fill" => {
                        state
                            .fill(
                                &context,
                                &opened.page,
                                &snapshot.document,
                                element,
                                "text",
                                &NeverCancel,
                            )
                            .await
                    }
                    "credential" => {
                        state
                            .fill_credential(
                                &context,
                                &opened.page,
                                &snapshot.document,
                                element,
                                "login",
                                &NeverCancel,
                            )
                            .await
                    }
                    "press" => {
                        state
                            .press(
                                &context,
                                &opened.page,
                                &snapshot.document,
                                "Enter",
                                &NeverCancel,
                            )
                            .await
                    }
                    _ => state.wait(&context, &opened.page, 0, &NeverCancel).await,
                };
                let (_, page) = state.active_page(&context, &opened.page).await.unwrap();
                let document = page.document.lock().await;
                assert!(document.token.is_none(), "{action}");
                assert!(document.origin.is_none(), "{action}");
                if final_url == Some("file:///secret") {
                    assert_eq!(
                        result.unwrap_err().0.code,
                        "invalid_provider_result",
                        "{action}"
                    );
                    assert!(document.url.is_none(), "{action}");
                } else {
                    result.unwrap();
                    assert_eq!(document.url.as_deref(), final_url, "{action}");
                }
                let uncertain = document.uncertain;
                drop(document);
                if !uncertain {
                    let (shot, _) = state
                        .screenshot(&context, &opened.page, &NeverCancel)
                        .await
                        .unwrap();
                    assert_eq!(
                        shot.url.as_deref(),
                        final_url.filter(|url| *url != "file:///secret"),
                        "{action}"
                    );
                    assert_eq!(shot.url_observation, "last_confirmed");
                    assert_eq!(
                        state
                            .press(
                                &context,
                                &opened.page,
                                &snapshot.document,
                                "Enter",
                                &NeverCancel
                            )
                            .await
                            .unwrap_err()
                            .0
                            .code,
                        "stale_element"
                    );
                } else {
                    assert_eq!(
                        state
                            .screenshot(&context, &opened.page, &NeverCancel)
                            .await
                            .unwrap_err()
                            .0
                            .code,
                        "outcome_unknown"
                    );
                    state
                        .snapshot(&context, &opened.page, &NeverCancel)
                        .await
                        .unwrap();
                }
            }
        }
    }

    #[tokio::test]
    async fn navigate_only_caches_a_validated_provider_confirmation() {
        let root = tempfile::tempdir().unwrap();
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let (state, _) = artifact_state(root.path(), provider.clone(), &run).await;
        let context = make_context(run);
        let (page_id, snapshot) = open_document(&state, &context, "https://allowed.example").await;
        for final_url in [
            Some("https://other.example/next"),
            None,
            Some("file:///secret"),
        ] {
            if let Some(url) = final_url {
                provider.return_next_action_url(url);
            } else {
                provider.omit_navigation_url.store(true, Ordering::Release);
            }
            let result = state
                .navigate(&context, &page_id, "https://allowed.example", &NeverCancel)
                .await;
            if final_url == Some("file:///secret") {
                assert_eq!(result.unwrap_err().0.code, "invalid_provider_result");
            } else {
                result.unwrap();
            }
            let (shot, _) = state
                .screenshot(&context, &page_id, &NeverCancel)
                .await
                .unwrap();
            assert_eq!(
                shot.url.as_deref(),
                final_url.filter(|url| *url != "file:///secret")
            );
            let (_, page) = state.active_page(&context, &page_id).await.unwrap();
            let document = page.document.lock().await;
            assert!(document.origin.is_none());
            assert!(document.token.is_none());
        }
        assert_eq!(
            state
                .press(
                    &context,
                    &page_id,
                    &snapshot.document,
                    "Enter",
                    &NeverCancel
                )
                .await
                .unwrap_err()
                .0
                .code,
            "stale_element"
        );
    }

    #[tokio::test]
    async fn unknown_action_clears_last_confirmed_url() {
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = make_state(provider.clone(), &run);
        let context = make_context(run);
        let (page_id, snapshot) = open_document(&state, &context, "https://allowed.example").await;
        provider.fail_next_unknown();
        assert_eq!(
            state
                .click(
                    &context,
                    &page_id,
                    &snapshot.document,
                    &snapshot.elements[0].element_ref,
                    None,
                    &NeverCancel
                )
                .await
                .unwrap_err()
                .0
                .code,
            "outcome_unknown"
        );
        let (_, page) = state.active_page(&context, &page_id).await.unwrap();
        let document = page.document.lock().await;
        assert!(document.url.is_none());
        assert!(document.origin.is_none());
        assert!(document.token.is_none());
        assert!(document.uncertain);
    }

    #[tokio::test]
    async fn screenshot_artifact_is_usable_for_upload_and_holds_its_lease() {
        let root = tempfile::tempdir().unwrap();
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let (state, outputs) = artifact_state(root.path(), provider.clone(), &run).await;
        let context = make_context(run);
        let (page, _) = open_document(&state, &context, "http://allowed.example/shot").await;
        let (view, sealed) = state
            .screenshot(&context, &page, &NeverCancel)
            .await
            .unwrap();
        let reference = view.media_ref.clone().unwrap();
        assert_eq!(view.model_image_capability, "disabled");
        assert!(view.masked);
        assert!(view.model_image_status.contains("artifact_saved"));
        assert!(view.model_image_status.contains("not injected"));
        assert_eq!(view.artifact_ref.as_deref(), Some(reference.as_str()));
        assert!(view.filename.starts_with("screenshot-"));
        assert_eq!(view.page, page);
        assert!(view.document_revision >= 1);
        assert!(sealed.is_some());
        // The lease held by the captured result prevents GC from reclaiming it.
        assert_eq!(
            outputs
                .collect_garbage(&std::collections::BTreeSet::new(), 8)
                .unwrap()
                .leased,
            1
        );
        assert!(
            !state
                .load_artifact(&context, &reference)
                .unwrap()
                .is_empty()
        );
        drop(sealed);
        assert_eq!(
            outputs
                .collect_garbage(&std::collections::BTreeSet::new(), 8)
                .unwrap()
                .removed,
            1
        );
        assert_eq!(
            state
                .load_artifact(&context, &reference)
                .unwrap_err()
                .0
                .code,
            "media_unavailable"
        );
    }

    #[tokio::test]
    async fn screenshot_artifact_survives_reopen_and_is_pinned_by_tool_history() {
        let root = tempfile::tempdir().unwrap();
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let (state, outputs) = artifact_state(root.path(), provider, &run).await;
        let context = make_context(run.clone());
        let (page, _) = open_document(&state, &context, "http://allowed.example/shot").await;
        let (view, sealed) = state
            .screenshot(&context, &page, &NeverCancel)
            .await
            .unwrap();
        let reference = view.media_ref.clone().unwrap();
        let result = crate::contract::ToolResult {
            ok: true,
            data: Some(
                serde_json::json!({"source": "untrusted_browser", "screenshot": {"mime": view.mime, "bytes": view.bytes, "sha256": view.sha256}}),
            ),
            error: None,
            truncated: false,
            media_refs: vec![reference.clone()],
        };
        let history = [crate::contract::HistoryItem::ToolResult {
            call_id: "shot-call".into(),
            result: result.clone(),
        }];
        assert!(
            crate::media::history_references(&history).contains(&reference),
            "host media refs must remain GC pins"
        );
        drop(sealed);
        drop(state);
        let reopened = Arc::new(
            OutputStore::open(
                outputs.protected_path(),
                crate::output_store::OutputLimits::default(),
                Arc::new(SecretRegistry::default()),
            )
            .unwrap(),
        );
        let media = reopened.load_media(run.session_id, &reference).unwrap();
        assert_eq!(media.kind, crate::media::MediaKind::Image);
        assert_eq!(media.mime, "image/png");
        let capabilities = crate::config::MediaCapabilities {
            image: true,
            audio: false,
            video: false,
        };
        let attachments =
            crate::media::load_history(&history, Some(&reopened), run.session_id, capabilities)
                .unwrap();
        assert_eq!(attachments.tool_result_images(&result).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn screenshot_capability_requires_masking_support() {
        let root = tempfile::tempdir().unwrap();
        let provider = Arc::new(FixtureProvider::new(BrowserFacts {
            provider: "fixture".into(),
            screenshots: true,
            screenshot_masking: false,
            downloads: true,
            uploads: true,
        }));
        let run = make_run();
        let (state, outputs) = artifact_state(root.path(), provider, &run).await;
        let context = make_context(run);
        let (page, _) = open_document(&state, &context, "http://allowed.example/shot").await;
        let error = state
            .screenshot(&context, &page, &NeverCancel)
            .await
            .unwrap_err();
        assert_eq!(error.0.code, "browser_capability_disabled");
        // Nothing was persisted for an unconfirmed capability.
        assert_eq!(stored_media_files(&outputs), 0);
    }

    #[tokio::test]
    async fn unmasked_provider_result_is_never_persisted() {
        let root = tempfile::tempdir().unwrap();
        let provider = Arc::new(FixtureProvider::full().with_unmasked_screenshot());
        let run = make_run();
        let (state, outputs) = artifact_state(root.path(), provider, &run).await;
        let context = make_context(run);
        let (page, _) = open_document(&state, &context, "http://allowed.example/shot").await;
        let error = state
            .screenshot(&context, &page, &NeverCancel)
            .await
            .unwrap_err();
        assert_eq!(error.0.code, "browser_capability_disabled");
        assert_eq!(stored_media_files(&outputs), 0);
    }

    #[tokio::test]
    async fn screenshot_registration_requires_the_file_broker_and_artifact_store() {
        let root = tempfile::tempdir().unwrap();
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        // Facts alone are insufficient: without a configured file broker and
        // artifact store the tool is neither registered nor executable.
        let state = make_state(provider.clone(), &run);
        assert!(!state.screenshot_available());
        let context = make_context(run.clone());
        let opened = state.open(&context, &NeverCancel).await.unwrap();
        let error = state
            .screenshot(&context, &opened.page, &NeverCancel)
            .await
            .unwrap_err();
        assert_eq!(error.0.code, "browser_capability_disabled");

        let (state, _outputs) = artifact_state(root.path(), provider, &run).await;
        assert!(state.screenshot_available());

        let outputs = Arc::new(
            OutputStore::open(
                root.path().join("disabled-outputs"),
                crate::output_store::OutputLimits::default(),
                Arc::new(SecretRegistry::default()),
            )
            .unwrap(),
        );
        let mut settings = BrowserSettings {
            enabled: true,
            ..Default::default()
        };
        settings.files.enabled = false;
        let disabled = BrowserState::new(settings, Arc::new(FixtureProvider::full()), run).unwrap();
        // `with_artifacts` is only reached by the host when files are enabled,
        // but the predicate itself must still reject the configuration.
        let disabled = disabled.with_artifacts(outputs).unwrap();
        assert!(!disabled.screenshot_available());
    }

    #[tokio::test]
    async fn screenshot_metadata_reports_the_profile_capability_declaration() {
        let root = tempfile::tempdir().unwrap();
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let (state, _outputs) = artifact_state(root.path(), provider, &run).await;
        let state = state.with_model_image_capability(true);
        let context = make_context(run);
        let (page, _) = open_document(&state, &context, "http://allowed.example/shot").await;
        let (view, _sealed) = state
            .screenshot(&context, &page, &NeverCancel)
            .await
            .unwrap();
        assert_eq!(view.model_image_capability, "enabled");
        assert!(view.model_image_status.contains("image input enabled"));
        assert!(view.model_image_status.contains("injected only if"));
        // Data carries the public artifact reference but never the host-only
        // durable reference.
        let encoded = serde_json::to_value(&view).unwrap();
        assert!(encoded.get("media_ref").is_none());
        assert_eq!(encoded["artifact_ref"].as_str(), view.media_ref.as_deref());
    }

    #[tokio::test]
    async fn screenshot_tool_result_uses_the_nested_host_shape() {
        let root = tempfile::tempdir().unwrap();
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let (state, _outputs) = artifact_state(root.path(), provider, &run).await;
        let state = state.with_model_image_capability(true);
        let context = make_context(run);
        let (page, _) = open_document(&state, &context, "http://allowed.example/shot").await;
        let tool = BrowserTool {
            state: Arc::new(state),
            kind: BrowserToolKind::Screenshot,
        };
        let call = ToolCall {
            call_id: "shot-call".into(),
            name: "browser_screenshot".into(),
            arguments: serde_json::json!({"page": page}).to_string(),
        };
        let capture = tool
            .execute_captured(&call, &context, &NeverCancel, None)
            .await
            .unwrap();
        assert_eq!(capture.result.media_refs.len(), 1);
        let data = capture.result.data.as_ref().unwrap();
        assert!(data.get("page").is_none());
        assert!(data.get("document_revision").is_none());
        let screenshot = data.get("screenshot").unwrap();
        assert_eq!(screenshot["page"], page);
        assert!(screenshot["document_revision"].as_u64().unwrap() >= 1);
        assert_eq!(screenshot["model_image_capability"], "enabled");
        assert!(screenshot.get("media_ref").is_none());
        assert!(screenshot["artifact_ref"].as_str().is_some());
        assert!(screenshot["bytes"].as_u64().unwrap() > 0);
        assert!(screenshot["sha256"].as_str().unwrap().len() == 64);
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
    #[tokio::test]
    async fn credential_fill_rejects_elements_inside_a_child_frame() {
        let root = tempfile::tempdir().unwrap();
        let origin = "https://allowed.example";
        let registry = Arc::new(SecretRegistry::default());
        let store =
            FileCredentialStore::open(root.path().join("credentials"), registry.clone()).unwrap();
        let scope =
            CredentialScope::for_browser("login", &NormalizedOrigin::parse(origin).unwrap())
                .unwrap();
        store
            .create(
                scope,
                CredentialValue::Secret(SecretValue::new("super-secret-value").unwrap()),
            )
            .await
            .unwrap();
        let settings = BrowserSettings {
            credentials: BrowserCredentialSettings {
                enabled: true,
                store: Some(root.path().join("credentials")),
                slots: vec![BrowserCredentialSlot {
                    alias: "login".into(),
                    credential_ref: store.list().await.unwrap()[0].reference,
                    origin: origin.into(),
                    purpose: BrowserCredentialPurpose::FormFill,
                    max_uses: 1,
                }],
            },
            ..allowed_settings(origin)
        };
        let provider = Arc::new(FixtureProvider::full().with_framed_element());
        let run = make_run();
        let broker = BrowserCredentialBroker::new(&settings.credentials, store.clone()).unwrap();
        let state = BrowserState::new(settings, provider.clone(), run.clone())
            .unwrap()
            .with_credentials(broker);
        let context = make_context(run);
        let (_page, snapshot) = open_document(&state, &context, &format!("{origin}/login")).await;
        // A normal snapshot still exposes the reference for text fills...
        assert!(
            state
                .fill(
                    &context,
                    &snapshot.page,
                    &snapshot.document,
                    &snapshot.elements[0].element_ref,
                    "visible text",
                    &NeverCancel,
                )
                .await
                .is_ok()
        );
        let (_page, snapshot) = open_document(&state, &context, &format!("{origin}/login")).await;
        // ...but credential material never crosses a frame boundary.
        let error = state
            .fill_credential(
                &context,
                &snapshot.page,
                &snapshot.document,
                &snapshot.elements[0].element_ref,
                "login",
                &NeverCancel,
            )
            .await
            .unwrap_err();
        assert_eq!(error.0.code, "credential_frame_denied");
    }

    #[tokio::test]
    async fn download_errors_conservatively_invalidate_the_document() {
        let root = tempfile::tempdir().unwrap();
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let (state, _outputs) = artifact_state(root.path(), provider.clone(), &run).await;
        let context = make_context(run);
        let (page, mut snapshot) =
            open_document(&state, &context, "http://allowed.example/files").await;

        for code in [
            "download_missing",
            "invalid_provider_result",
            "file_path_denied",
            "file_too_large",
            "file_broker",
        ] {
            provider.fail_next_download(code, false);
            let error = state
                .download(
                    &context,
                    &page,
                    &snapshot.document,
                    &snapshot.elements[0].element_ref,
                    &NeverCancel,
                )
                .await
                .unwrap_err();
            assert_eq!(error.0.code, code);
            let (shot, _) = state
                .screenshot(&context, &page, &NeverCancel)
                .await
                .unwrap();
            assert!(shot.url.is_none());
            // A known failure keeps the page usable only through a fresh
            // observation; the old document must not be replayed.
            assert_eq!(
                state
                    .download(
                        &context,
                        &page,
                        &snapshot.document,
                        &snapshot.elements[0].element_ref,
                        &NeverCancel,
                    )
                    .await
                    .unwrap_err()
                    .0
                    .code,
                "stale_element"
            );
            snapshot = state.snapshot(&context, &page, &NeverCancel).await.unwrap();
        }

        // An unknown outcome stays unknown until a fresh snapshot.
        provider.fail_next_download("download_missing", true);
        assert_eq!(
            state
                .download(
                    &context,
                    &page,
                    &snapshot.document,
                    &snapshot.elements[0].element_ref,
                    &NeverCancel,
                )
                .await
                .unwrap_err()
                .0
                .code,
            "outcome_unknown"
        );
        assert_eq!(
            state
                .download(
                    &context,
                    &page,
                    &snapshot.document,
                    &snapshot.elements[0].element_ref,
                    &NeverCancel,
                )
                .await
                .unwrap_err()
                .0
                .code,
            "outcome_unknown"
        );
        let recovered = state.snapshot(&context, &page, &NeverCancel).await.unwrap();
        assert!(!recovered.document.is_empty());
    }

    #[tokio::test]
    async fn shutdown_cancels_an_in_flight_wait_instead_of_waiting_for_the_lock() {
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = Arc::new(make_state(provider, &run));
        let context = make_context(run);
        let opened = state.open(&context, &NeverCancel).await.unwrap();
        let waiter = {
            let state = state.clone();
            let context = context.clone();
            let page = opened.page.clone();
            tokio::spawn(async move { state.wait(&context, &page, 30_000, &NeverCancel).await })
        };
        // Give the wait time to enter the provider with the document lock held.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let started = std::time::Instant::now();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            state.shutdown(&NeverCancel),
        )
        .await
        .expect("shutdown waited for the full wait deadline")
        .expect("shutdown");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "shutdown did not interrupt the in-flight wait"
        );
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("wait task did not finish")
            .unwrap();
        assert_eq!(result.unwrap_err().0.code, "cancelled");
    }

    #[tokio::test]
    async fn background_binding_rearms_after_plan_shutdown_and_rejects_closed_jobs() {
        use crate::{
            approvals::{ApprovalSetup, PermissionRevision, Preauthorizations},
            background::JobStatus,
            boundary::Store,
            contract::{ModelProtocol, Session, WorkMode},
            memory::MemoryStore,
            mode::ModeController,
            permissions::{PermissionConfig, Policy},
            tools::{
                exec::ExecConfig,
                read::{ReadLimits, ReadTools},
            },
        };

        let root = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
        let run = Run {
            id: RunId::new(),
            session_id: SessionId::new(),
            work_mode: WorkMode::Work,
            permissions: crate::contract::PermissionMode::Bypass,
        };
        let session = Session {
            id: run.session_id,
            workspace: root.path().to_path_buf(),
            work_mode: WorkMode::Work,
            profile: "fixture".into(),
            protocol: ModelProtocol::ChatCompletions,
            model: "fixture".into(),
            history: Vec::new(),
        };
        store
            .commit(&[
                crate::boundary::StoreChange::Session(session),
                crate::boundary::StoreChange::RunStarted(run.clone()),
            ])
            .await
            .unwrap();
        let modes = ModeController::new(WorkMode::Work);
        let dispatcher = Arc::new(
            ToolDispatcher::new(
                Arc::new(Policy::new(root.path(), PermissionConfig::default()).unwrap()),
                modes.clone(),
                PermissionRevision::new(modes.clone(), crate::contract::PermissionMode::Bypass),
                store,
                ApprovalSetup {
                    run_id: run.id,
                    session_id: run.session_id,
                    initial_permissions: crate::contract::PermissionMode::Bypass,
                    preauthorizations: Preauthorizations::default(),
                    known_secrets: Vec::new(),
                    cancellation: Arc::new(NeverCancel),
                },
                Arc::new(ReadTools::new(root.path(), &[], true, ReadLimits::default()).unwrap()),
                ExecConfig::default(),
            )
            .unwrap(),
        );
        let jobs = Arc::new(
            BackgroundJobs::new(dispatcher, 1, 2)
                .unwrap()
                .with_tool_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
        );
        let state = Arc::new(
            BrowserState::new_with_services(
                BrowserSettings {
                    enabled: true,
                    ..Default::default()
                },
                Arc::new(FixtureProvider::full()),
                run.clone(),
                None,
                Some(jobs.clone()),
                Arc::new(SecretRegistry::default()),
                crate::config::MediaCapabilities::default(),
            )
            .unwrap(),
        );
        let context = ToolContext {
            run: run.clone(),
            workspace: root.path().to_path_buf(),
        };
        let tool = BrowserTool {
            state: state.clone(),
            kind: BrowserToolKind::Navigate,
        };
        let opened = state.open(&context, &NeverCancel).await.unwrap();
        let call = |page: &str, call_id: &str| ToolCall {
            call_id: call_id.into(),
            name: "browser_navigate".into(),
            arguments: serde_json::json!({
                "page": page,
                "url": "http://allowed.example/next",
                "background": true,
            })
            .to_string(),
        };

        // PLAN cleanup: the provider and background gate close. A background
        // action must fail explicitly rather than silently starting.
        state.shutdown(&NeverCancel).await.unwrap();
        let error = tool
            .execute_captured(
                &call(&opened.page, "bg-plan-closed"),
                &context,
                &NeverCancel,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(error.0.code, "unsupported");

        // An explicit open re-arms the same shared supervisor.
        let reopened = state.open(&context, &NeverCancel).await.unwrap();
        let captured = tool
            .execute_captured(
                &call(&reopened.page, "bg-after-plan"),
                &context,
                &NeverCancel,
                None,
            )
            .await
            .unwrap();
        let data = captured.result.data.unwrap();
        assert_eq!(data["status"], "started");
        let job_id = data["job_id"].as_str().unwrap().to_owned();
        let status = tokio::time::timeout(std::time::Duration::from_secs(5), jobs.wait(&job_id))
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(status, JobStatus::Finished(result) if result.ok));

        // A closed supervisor is never reused, even after a later open.
        jobs.shutdown().await.unwrap();
        let third = state.open(&context, &NeverCancel).await.unwrap();
        let error = tool
            .execute_captured(
                &call(&third.page, "bg-closed-jobs"),
                &context,
                &NeverCancel,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(error.0.code, "unsupported");
    }

    #[tokio::test]
    async fn screenshot_enforces_observed_url_origin_and_document_consistency() {
        let root = tempfile::tempdir().unwrap();
        let outputs = Arc::new(
            OutputStore::open(
                root.path().join("outputs"),
                crate::output_store::OutputLimits::default(),
                Arc::new(SecretRegistry::default()),
            )
            .unwrap(),
        );
        let provider = Arc::new(FixtureProvider::full());
        let run = make_run();
        let state = BrowserState::new(
            BrowserSettings {
                enabled: true,
                network: crate::browser::network::BrowserNetworkSettings {
                    enabled: true,
                    allowed_origins: vec!["http://127.0.0.1:8080".into()],
                    localhost_development_origin: Some("http://127.0.0.1:8080".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
            provider.clone(),
            run.clone(),
        )
        .unwrap()
        .with_artifacts(outputs)
        .unwrap();
        let context = make_context(run);
        let cancel = NeverCancel;
        let opened = state.open(&context, &cancel).await.unwrap();
        state
            .navigate(
                &context,
                &opened.page,
                "http://127.0.0.1:8080/login",
                &cancel,
            )
            .await
            .unwrap();
        let snap = state
            .snapshot(&context, &opened.page, &cancel)
            .await
            .unwrap();

        // Matching observed URL succeeds and preserves confirmed URL.
        *provider.override_screenshot_url.lock().unwrap() =
            Some("http://127.0.0.1:8080/login".into());
        let shot = state
            .screenshot(&context, &opened.page, &cancel)
            .await
            .unwrap();
        assert_eq!(shot.0.url.as_deref(), Some("http://127.0.0.1:8080/login"));
        assert_eq!(shot.0.url_observation, "last_confirmed");
        assert_eq!(shot.0.mime, "image/png");
        assert!(shot.1.is_some());

        // URL drift within allowed origin fails as stale_document and invalidates state.
        *provider.override_screenshot_url.lock().unwrap() =
            Some("http://127.0.0.1:8080/other".into());
        let err = state
            .screenshot(&context, &opened.page, &cancel)
            .await
            .unwrap_err();
        assert_eq!(err.0.code, "stale_document");
        let stale_click = state
            .click(
                &context,
                &opened.page,
                &snap.document,
                &snap.elements[0].element_ref,
                None,
                &cancel,
            )
            .await
            .unwrap_err();
        assert_eq!(stale_click.0.code, "stale_element");

        // Disallowed origin fails as network_final_origin_denied and invalidates state.
        state
            .navigate(
                &context,
                &opened.page,
                "http://127.0.0.1:8080/login",
                &cancel,
            )
            .await
            .unwrap();
        *provider.override_screenshot_url.lock().unwrap() =
            Some("https://outside.example/phish".into());
        let err = state
            .screenshot(&context, &opened.page, &cancel)
            .await
            .unwrap_err();
        assert_eq!(err.0.code, "network_final_origin_denied");

        // Non-HTTP(S)/non-blank URL fails as invalid_provider_result.
        *provider.override_screenshot_url.lock().unwrap() = Some("file:///etc/passwd".into());
        let err = state
            .screenshot(&context, &opened.page, &cancel)
            .await
            .unwrap_err();
        assert_eq!(err.0.code, "invalid_provider_result");
    }
}
