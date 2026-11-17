use super::*;

#[derive(Debug, Clone, Copy)]
pub(super) enum BrowserToolKind {
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

pub(super) struct BrowserTool {
    pub(super) state: Arc<BrowserState>,
    pub(super) kind: BrowserToolKind,
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

    fn json_captured(
        value: serde_json::Value,
        media_refs: Vec<String>,
        media: Vec<SealedMedia>,
    ) -> CapturedToolResult {
        CapturedToolResult {
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
        }
    }

    fn action_result_json(view: &ActionResultView) -> serde_json::Value {
        serde_json::json!({
            "source": UNTRUSTED_BROWSER,
            "page": view.page,
            "url": view.url,
            "detail": view.detail,
            "document_stale": view.document_stale,
            "document_revision": view.document_revision,
        })
    }

    async fn execute_navigate(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        cancel: &dyn Cancellation,
    ) -> Result<CapturedToolResult, BoundaryError> {
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
                args.page,
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
        Ok(Self::json_captured(
            serde_json::json!({
                "source": UNTRUSTED_BROWSER,
                "page": view.page,
                "url": view.url,
                "document_stale": view.document_stale,
            }),
            Vec::new(),
            Vec::new(),
        ))
    }

    async fn execute_snapshot(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        cancel: &dyn Cancellation,
        outputs: Option<&OutputStore>,
    ) -> Result<CapturedToolResult, BoundaryError> {
        let page = parse_page(call)?;
        let view = self.state.snapshot(context, &page, cancel).await?;
        // Capture the complete host projection so a long snapshot retains
        // page/document/elements/url instead of only its body.
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
            "source": UNTRUSTED_BROWSER,
            "page": view.page,
            "document": view.document,
            "document_revision": view.document_revision,
            "url": view.url,
            "title": view.title,
            "elements": view.elements,
            "truncated": view.truncated,
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
            // zero-byte retention) still returns a bounded inline observation
            // instead of an empty success.
            let (text, text_truncated) = bounded_text(&view.text, INLINE_SNAPSHOT_BYTES);
            inline_truncated = text_truncated;
            data["text"] = serde_json::Value::String(text);
        }
        let captured_truncated = metadata.iter().any(|item| !item.artifact_complete)
            || (capture_lost && !metadata.is_empty());
        let truncated = view.truncated || inline_truncated || captured_truncated;
        data["truncated"] = serde_json::Value::Bool(truncated);
        Ok(CapturedToolResult {
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
        })
    }

    async fn execute_click(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        cancel: &dyn Cancellation,
    ) -> Result<CapturedToolResult, BoundaryError> {
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
        Ok(Self::json_captured(
            Self::action_result_json(&view),
            Vec::new(),
            Vec::new(),
        ))
    }

    async fn execute_fill(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        cancel: &dyn Cancellation,
    ) -> Result<CapturedToolResult, BoundaryError> {
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
        Ok(Self::json_captured(
            serde_json::json!({
                "source": UNTRUSTED_BROWSER,
                "page": view.page,
                "url": view.url,
                "detail": view.detail,
                "document_stale": view.document_stale,
                "document_revision": view.document_revision,
                "credential_slot": view.credential_slot,
                "credential_uses_remaining": view.credential_uses_remaining,
            }),
            Vec::new(),
            Vec::new(),
        ))
    }

    async fn execute_press(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        cancel: &dyn Cancellation,
    ) -> Result<CapturedToolResult, BoundaryError> {
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
        Ok(Self::json_captured(
            Self::action_result_json(&view),
            Vec::new(),
            Vec::new(),
        ))
    }

    async fn execute_wait(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        cancel: &dyn Cancellation,
    ) -> Result<CapturedToolResult, BoundaryError> {
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
                args.page,
                move |state, context, cancel| {
                    Box::pin(async move {
                        let view = state
                            .wait(&context, &page, milliseconds, cancel.as_ref())
                            .await?;
                        Ok((Self::action_result_json(&view), Vec::new(), Vec::new()))
                    })
                },
            );
        }
        let view = self
            .state
            .wait(context, &args.page, args.milliseconds, cancel)
            .await?;
        Ok(Self::json_captured(
            Self::action_result_json(&view),
            Vec::new(),
            Vec::new(),
        ))
    }

    async fn execute_screenshot(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        cancel: &dyn Cancellation,
    ) -> Result<CapturedToolResult, BoundaryError> {
        let page = parse_page(call)?;
        let (view, sealed) = self.state.screenshot(context, &page, cancel).await?;
        let media_refs = view.media_ref.clone().into_iter().collect();
        let encoded = serde_json::to_value(&view).map_err(|_| {
            failure(
                "serialization_error",
                "Browser screenshot cannot be encoded",
            )
        })?;
        Ok(Self::json_captured(
            serde_json::json!({
                "source": UNTRUSTED_BROWSER,
                "screenshot": encoded,
            }),
            media_refs,
            sealed.into_iter().collect(),
        ))
    }

    async fn execute_download(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        cancel: &dyn Cancellation,
    ) -> Result<CapturedToolResult, BoundaryError> {
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
                args.page,
                move |state, context, cancel| {
                    Box::pin(async move {
                        let (view, sealed) = state
                            .download(&context, &page, &document, &element, cancel.as_ref())
                            .await?;
                        let media_refs = view.media_ref.clone().into_iter().collect::<Vec<_>>();
                        // The supervisor holds the artifact lease in its job
                        // state until the Run ends, so GC cannot reclaim it
                        // before status/upload.
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
        let media_refs = view.media_ref.clone().into_iter().collect();
        let encoded = serde_json::to_value(view)
            .map_err(|_| failure("serialization_error", "Browser download cannot be encoded"))?;
        Ok(Self::json_captured(
            serde_json::json!({
                "source": UNTRUSTED_BROWSER,
                "download": encoded,
            }),
            media_refs,
            sealed.into_iter().collect(),
        ))
    }

    async fn execute_upload(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        cancel: &dyn Cancellation,
    ) -> Result<CapturedToolResult, BoundaryError> {
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
        let media_refs = vec![view.artifact_ref.clone()];
        let encoded = serde_json::to_value(view)
            .map_err(|_| failure("serialization_error", "Browser upload cannot be encoded"))?;
        Ok(Self::json_captured(
            serde_json::json!({
                "source": UNTRUSTED_BROWSER,
                "upload": encoded,
            }),
            media_refs,
            Vec::new(),
        ))
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
        match self.kind {
            BrowserToolKind::Open => {
                let view = self.state.open(context, cancel).await?;
                Ok(Self::json_captured(
                    serde_json::json!({
                        "source": UNTRUSTED_BROWSER,
                        "page": view.page,
                        "url": view.url,
                    }),
                    Vec::new(),
                    Vec::new(),
                ))
            }
            BrowserToolKind::Close => {
                self.state.close(context, cancel).await?;
                Ok(Self::json_captured(
                    serde_json::json!({
                        "source": UNTRUSTED_BROWSER,
                        "closed": true,
                    }),
                    Vec::new(),
                    Vec::new(),
                ))
            }
            BrowserToolKind::Navigate => self.execute_navigate(call, context, cancel).await,
            BrowserToolKind::Snapshot => {
                self.execute_snapshot(call, context, cancel, outputs).await
            }
            BrowserToolKind::Click => self.execute_click(call, context, cancel).await,
            BrowserToolKind::Fill => self.execute_fill(call, context, cancel).await,
            BrowserToolKind::Press => self.execute_press(call, context, cancel).await,
            BrowserToolKind::Wait => self.execute_wait(call, context, cancel).await,
            BrowserToolKind::Screenshot => self.execute_screenshot(call, context, cancel).await,
            BrowserToolKind::Download => self.execute_download(call, context, cancel).await,
            BrowserToolKind::Upload => self.execute_upload(call, context, cancel).await,
        }
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
