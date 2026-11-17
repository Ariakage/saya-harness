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

mod state;
#[cfg(test)]
mod tests;
mod tools;

pub use state::{
    ActionResultView, BrowserState, DownloadView, ElementView, NavigateView, OpenView,
    ScreenshotView, SnapshotView, UploadView,
};
#[cfg(test)]
pub use tests::FixtureProvider;
pub(crate) use tools::register_tools;

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
