use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ElementTarget {
    key: String,
    /// Present only when the element lives inside a child frame.
    frame: Option<String>,
}

#[derive(Default)]
pub(super) struct PageDocument {
    pub(super) token: Option<String>,
    elements: BTreeMap<String, ElementTarget>,
    revision: u64,
    /// Exact canonical origin observed with the current document token.
    pub(super) origin: Option<crate::network::NormalizedOrigin>,
    /// Last provider-confirmed URL for this page. Used only for host metadata
    /// and never as an authorization decision.
    pub(super) url: Option<String>,
    /// An action on this page may have been dispatched without a confirmed
    /// outcome. Only a fresh snapshot of this page, or a complete close, clears
    /// it. It must never be cleared by another page's snapshot or a new open.
    pub(super) uncertain: bool,
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

pub(super) struct Page {
    key: String,
    pub(super) document: AsyncMutex<PageDocument>,
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
    pub(super) facts: BrowserFacts,
    pub(super) settings: BrowserSettings,
    credentials: Option<BrowserCredentialBroker>,
    artifacts: Option<BrowserArtifactStore>,
    /// The profile image capability observed when this Run was configured.
    /// Screenshot metadata reports this declaration, never a per-request fact.
    model_image_capability: bool,
    /// Weak supervisor handle prevents the
    /// BrowserState → BackgroundJobs → dispatcher → BrowserTool → BrowserState
    /// ownership cycle. The binding is retained across PLAN cleanup and
    /// re-armed by an explicit `open`; a closed supervisor is never reused.
    pub(super) background: std::sync::Mutex<Option<std::sync::Weak<BackgroundJobs>>>,
    /// True only while a usable provider/page lifecycle exists. PLAN cleanup
    /// disables background starts until a later explicit `open`.
    pub(super) background_enabled: AtomicBool,
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

    pub(super) async fn active_page(
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
