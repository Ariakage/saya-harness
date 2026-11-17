use super::tools::{BrowserTool, BrowserToolKind};
use super::*;
use std::sync::{Mutex, atomic::AtomicU64};

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
        CredentialScope::for_browser("login", &NormalizedOrigin::parse(origin).unwrap()).unwrap();
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
        let (page, snapshot) = open_document(&state, &context, "http://allowed.example/file").await;
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
        CredentialScope::for_browser("login", &NormalizedOrigin::parse(origin).unwrap()).unwrap();
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
    *provider.override_screenshot_url.lock().unwrap() = Some("http://127.0.0.1:8080/login".into());
    let shot = state
        .screenshot(&context, &opened.page, &cancel)
        .await
        .unwrap();
    assert_eq!(shot.0.url.as_deref(), Some("http://127.0.0.1:8080/login"));
    assert_eq!(shot.0.url_observation, "last_confirmed");
    assert_eq!(shot.0.mime, "image/png");
    assert!(shot.1.is_some());

    // URL drift within allowed origin fails as stale_document and invalidates state.
    *provider.override_screenshot_url.lock().unwrap() = Some("http://127.0.0.1:8080/other".into());
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
