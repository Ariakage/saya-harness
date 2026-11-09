use crate::boundary::{
    Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation, StoreChange,
};
use crate::contract::{BoundaryError, ErrorInfo, HistoryItem};
use crate::credentials::SecretRegistry;
use serde::{Serialize, de::DeserializeOwned};
use std::sync::{Arc, RwLock};

pub(super) type SecretSource = Arc<RwLock<Option<Arc<SecretRegistry>>>>;
pub(super) fn source() -> SecretSource {
    Arc::new(RwLock::new(None))
}
pub(super) fn snapshot(source: &SecretSource) -> Result<Vec<String>, BoundaryError> {
    let registry = source.read().map_err(|_| failure())?.clone();
    registry
        .map(|registry| registry.snapshot())
        .unwrap_or_else(|| Ok(Vec::new()))
}
fn failure() -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "sensitive_data".into(),
            message: "Dynamic credential boundary is unavailable".into(),
        },
        None,
        None,
    )
}
pub(super) fn redact<T: Serialize + DeserializeOwned>(
    value: T,
    source: &SecretSource,
) -> Result<T, BoundaryError> {
    let secrets = snapshot(source)?;
    if secrets.is_empty() {
        return Ok(value);
    }
    let mut value = serde_json::to_value(value).map_err(|_| failure())?;
    crate::redaction::redact(&mut value, &secrets)?;
    serde_json::from_value(value).map_err(|_| failure())
}
fn require_clean<T: Serialize>(value: &T, source: &SecretSource) -> Result<(), BoundaryError> {
    let secrets = snapshot(source)?;
    if !secrets.is_empty()
        && crate::redaction::contains_secret(
            &serde_json::to_string(value).map_err(|_| failure())?,
            &secrets,
        )
    {
        return Err(failure());
    }
    Ok(())
}
fn history_item(item: HistoryItem, source: &SecretSource) -> Result<HistoryItem, BoundaryError> {
    match &item {
        HistoryItem::ToolCall(call) => require_clean(&(&call.call_id, &call.name), source)?,
        HistoryItem::ToolResult { call_id, .. } => require_clean(call_id, source)?,
        HistoryItem::ProviderItem { .. } => {
            require_clean(&item, source)?;
            return Ok(item);
        }
        _ => {}
    }
    redact(item, source)
}
fn session(
    mut session: crate::contract::Session,
    source: &SecretSource,
) -> Result<crate::contract::Session, BoundaryError> {
    let items = std::mem::take(&mut session.history);
    require_clean(&session, source)?;
    session.history = history(items, source)?;
    Ok(session)
}
pub(super) fn loaded_session(
    value: Option<crate::contract::Session>,
    source: &SecretSource,
) -> Result<Option<crate::contract::Session>, BoundaryError> {
    value.map(|value| session(value, source)).transpose()
}
pub(super) fn model_request(
    request: &ModelRequest,
    source: &SecretSource,
) -> Result<(), BoundaryError> {
    let secrets = snapshot(source)?;
    if secrets.is_empty() {
        return Ok(());
    }
    let tools=request.tools.iter().map(|tool|serde_json::json!({"name":tool.name,"description":tool.description,"schema":tool.input_schema})).collect::<Vec<_>>();
    let value = serde_json::json!({"instructions":request.instructions,"dynamic":request.dynamic_instructions,"history":request.history,"tools":tools});
    if crate::redaction::contains_secret(&value.to_string(), &secrets) {
        return Err(failure());
    }
    Ok(())
}
pub(super) fn changes(
    changes: &[StoreChange],
    source: &SecretSource,
) -> Result<Vec<StoreChange>, BoundaryError> {
    changes
        .iter()
        .cloned()
        .map(|change| {
            Ok(match change {
                StoreChange::Session(value) => StoreChange::Session(session(value, source)?),
                StoreChange::RunStarted(value) => StoreChange::RunStarted(redact(value, source)?),
                StoreChange::MemoryExposure(value) => {
                    require_clean(&value, source)?;
                    StoreChange::MemoryExposure(value)
                }
                StoreChange::ContextCheckpoint { run_id, checkpoint } => {
                    require_clean(&checkpoint, source)?;
                    StoreChange::ContextCheckpoint { run_id, checkpoint }
                }
                StoreChange::History { session_id, item } => StoreChange::History {
                    session_id,
                    item: history_item(item, source)?,
                },
                StoreChange::ProjectContext { run_id, record } => StoreChange::ProjectContext {
                    run_id,
                    record: redact(record, source)?,
                },
                StoreChange::Activity { run_id, payload } => StoreChange::Activity {
                    run_id,
                    payload: redact(payload, source)?,
                },
                StoreChange::RunMetrics { run_id, metrics } => StoreChange::RunMetrics {
                    run_id,
                    metrics: redact(metrics, source)?,
                },
                StoreChange::ToolIntent { run_id, call } => {
                    require_clean(&(&call.call_id, &call.name), source)?;
                    StoreChange::ToolIntent {
                        run_id,
                        call: redact(call, source)?,
                    }
                }
                StoreChange::ToolFinished {
                    run_id,
                    call_id,
                    result,
                } => {
                    require_clean(&call_id, source)?;
                    StoreChange::ToolFinished {
                        run_id,
                        call_id,
                        result: redact(result, source)?,
                    }
                }
                StoreChange::ToolState {
                    run_id,
                    call_id,
                    state,
                } => {
                    require_clean(&call_id, source)?;
                    StoreChange::ToolState {
                        run_id,
                        call_id,
                        state,
                    }
                }
                StoreChange::OutputReferences {
                    run_id,
                    call_id,
                    outputs,
                } => {
                    require_clean(&(&call_id, &outputs), source)?;
                    StoreChange::OutputReferences {
                        run_id,
                        call_id,
                        outputs,
                    }
                }
                StoreChange::ApprovalRequested(value) => {
                    StoreChange::ApprovalRequested(redact(value, source)?)
                }
                StoreChange::RunFinished(value) => StoreChange::RunFinished(redact(value, source)?),
                StoreChange::Event(value) => StoreChange::Event(redact(value, source)?),
                StoreChange::ReservedEvent(value) => {
                    StoreChange::ReservedEvent(redact(value, source)?)
                }
            })
        })
        .collect()
}
pub(super) fn history(
    items: Vec<HistoryItem>,
    source: &SecretSource,
) -> Result<Vec<HistoryItem>, BoundaryError> {
    items
        .into_iter()
        .map(|item| history_item(item, source))
        .collect()
}

// Authorization and execution must observe the exact arguments returned by the
// model. Only persisted/replayed history may contain a redacted copy.
pub(super) fn model_response(
    items: Vec<HistoryItem>,
    source: &SecretSource,
) -> Result<Vec<HistoryItem>, BoundaryError> {
    items
        .into_iter()
        .map(|item| match &item {
            HistoryItem::ToolCall(call) => {
                require_clean(&(&call.call_id, &call.name), source)?;
                Ok(item)
            }
            _ => history_item(item, source),
        })
        .collect()
}

pub(super) struct GuardedModel {
    inner: Arc<dyn Model>,
    source: SecretSource,
}
impl GuardedModel {
    pub(super) fn new(inner: Arc<dyn Model>, source: SecretSource) -> Self {
        Self { inner, source }
    }
}
impl Model for GuardedModel {
    fn request_counter(&self) -> Option<&dyn crate::context_builder::RequestCounter> {
        self.inner.request_counter()
    }
    fn check_start(&self, request: &ModelRequest) -> Result<(), BoundaryError> {
        model_request(request, &self.source)?;
        self.inner.check_start(request)
    }
    fn validate_session(&self, session: &crate::contract::Session) -> Result<(), BoundaryError> {
        self.inner.validate_session(session)
    }
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            model_request(request, &self.source)?;
            let stream = self.inner.start(request, cancel).await?;
            Ok(Box::new(GuardedStream {
                inner: stream,
                source: self.source.clone(),
                partial: String::new(),
                pending_error: None,
            }) as Box<dyn ModelStream>)
        })
    }
}
struct GuardedStream {
    inner: Box<dyn ModelStream>,
    source: SecretSource,
    partial: String,
    pending_error: Option<BoundaryError>,
}
impl ModelStream for GuardedStream {
    fn next<'a>(&'a mut self, cancel: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            if let Some(error) = self.pending_error.take() {
                return Err(error);
            }
            let guarded = self.source.read().map_err(|_| failure())?.is_some();
            if !guarded {
                return self.inner.next(cancel).await;
            }
            match self.inner.next(cancel).await {
                Ok(Some(ModelUpdate::TextDelta(text))) => {
                    if self.partial.len().saturating_add(text.len()) > 4 * 1024 * 1024 {
                        return Err(BoundaryError(
                            ErrorInfo {
                                code: "output_limit".into(),
                                message: "Buffered model text exceeds the credential-safe limit"
                                    .into(),
                            },
                            None,
                            None,
                        ));
                    }
                    self.partial.push_str(&text);
                    Ok(Some(ModelUpdate::Progress))
                }
                Ok(Some(ModelUpdate::ThinkingDelta { .. })) => Ok(Some(ModelUpdate::Progress)),
                Ok(Some(ModelUpdate::Finished { items, usage })) => {
                    self.partial.clear();
                    Ok(Some(ModelUpdate::Finished {
                        items: model_response(items, &self.source)?,
                        usage,
                    }))
                }
                Ok(None) if !self.partial.is_empty() => {
                    let partial = redact(std::mem::take(&mut self.partial), &self.source)?;
                    Ok(Some(ModelUpdate::TextDelta(partial)))
                }
                Err(mut error) => {
                    error.0 = redact(error.0, &self.source)?;
                    if let Some(result) = error.2.take() {
                        error.2 = Some(redact(result, &self.source)?);
                    }
                    if !self.partial.is_empty() {
                        let partial = redact(std::mem::take(&mut self.partial), &self.source)?;
                        self.pending_error = Some(error);
                        Ok(Some(ModelUpdate::TextDelta(partial)))
                    } else {
                        Err(error)
                    }
                }
                other => other,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{ModelProtocol, RunId, SessionId, ToolCall};
    use std::{
        collections::VecDeque,
        future::{Future, pending},
        pin::Pin,
    };
    struct Never;
    impl Cancellation for Never {
        fn is_cancelled(&self) -> bool {
            false
        }
        fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            Box::pin(pending())
        }
    }
    struct Steps(VecDeque<ModelUpdate>);
    impl ModelStream for Steps {
        fn next<'a>(&'a mut self, _: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
            Box::pin(async move { Ok(self.0.pop_front()) })
        }
    }
    fn guarded(updates: Vec<ModelUpdate>) -> (GuardedStream, Arc<SecretRegistry>) {
        let registry = Arc::new(SecretRegistry::default());
        let source = Arc::new(RwLock::new(Some(registry.clone())));
        (
            GuardedStream {
                inner: Box::new(Steps(updates.into())),
                source,
                partial: String::new(),
                pending_error: None,
            },
            registry,
        )
    }
    #[tokio::test]
    async fn model_output_buffers_fragments_and_observes_credentials_learned_during_the_turn() {
        let (mut stream, registry) = guarded(vec![
            ModelUpdate::TextDelta("credential-new-".into()),
            ModelUpdate::TextDelta("token".into()),
            ModelUpdate::ThinkingDelta {
                text: "credential-new-token".into(),
            },
            ModelUpdate::Finished {
                items: vec![HistoryItem::AssistantText("credential-new-token".into())],
                usage: None,
            },
        ]);
        assert!(matches!(
            stream.next(&Never).await.unwrap(),
            Some(ModelUpdate::Progress)
        ));
        registry.register(&["credential-new-token".into()]).unwrap();
        assert!(matches!(
            stream.next(&Never).await.unwrap(),
            Some(ModelUpdate::Progress)
        ));
        assert!(matches!(
            stream.next(&Never).await.unwrap(),
            Some(ModelUpdate::Progress)
        ));
        let Some(ModelUpdate::Finished { items, .. }) = stream.next(&Never).await.unwrap() else {
            panic!("expected finished response");
        };
        assert_eq!(items, vec![HistoryItem::AssistantText("[REDACTED]".into())]);
    }
    #[tokio::test]
    async fn incomplete_guarded_stream_keeps_only_sanitized_partial_output() {
        let (mut stream, registry) = guarded(vec![ModelUpdate::TextDelta(
            "prefix credential-secret suffix".into(),
        )]);
        registry.register(&["credential-secret".into()]).unwrap();
        assert!(matches!(
            stream.next(&Never).await.unwrap(),
            Some(ModelUpdate::Progress)
        ));
        assert!(
            matches!(stream.next(&Never).await.unwrap(),Some(ModelUpdate::TextDelta(value)) if value=="prefix [REDACTED] suffix")
        );
        assert!(stream.next(&Never).await.unwrap().is_none());
    }
    #[test]
    fn opaque_provider_data_and_tool_identifiers_fail_closed_instead_of_being_rewritten() {
        let source = source();
        let registry = Arc::new(SecretRegistry::default());
        registry.register(&["sensitive-call".into()]).unwrap();
        *source.write().unwrap() = Some(registry);
        let call = HistoryItem::ToolCall(ToolCall {
            call_id: "sensitive-call".into(),
            name: "read".into(),
            arguments: "{}".into(),
        });
        assert_eq!(
            history(vec![call], &source).unwrap_err().0.code,
            "sensitive_data"
        );
        let opaque = HistoryItem::ProviderItem {
            protocol: ModelProtocol::Responses,
            value: serde_json::json!({"opaque":"sensitive-call"}),
        };
        assert_eq!(
            history(vec![opaque], &source).unwrap_err().0.code,
            "sensitive_data"
        );
    }
    #[tokio::test]
    async fn tool_arguments_remain_exact_until_authorization_while_history_is_redacted() {
        let secret = "fixture\"secret\\tail".to_owned();
        let mut nested = serde_json::json!({"token":secret}).to_string();
        for _ in 0..8 {
            nested = serde_json::to_string(&nested).unwrap();
        }
        let call = HistoryItem::ToolCall(ToolCall {
            call_id: "credential-call".into(),
            name: "exec".into(),
            arguments: serde_json::json!({"env":{"PAYLOAD":{"source":"literal","value":nested}}})
                .to_string(),
        });
        let (mut stream, registry) = guarded(vec![ModelUpdate::Finished {
            items: vec![call.clone()],
            usage: None,
        }]);
        registry.register(std::slice::from_ref(&secret)).unwrap();
        let Some(ModelUpdate::Finished { items, .. }) = stream.next(&Never).await.unwrap() else {
            panic!("expected finished response");
        };
        let items = model_response(items, &stream.source).unwrap();
        assert_eq!(items, vec![call.clone()]);
        let stored = history(items.clone(), &stream.source).unwrap();
        assert!(!crate::redaction::contains_secret(
            &serde_json::to_string(&stored).unwrap(),
            std::slice::from_ref(&secret)
        ));
        assert_ne!(stored, items);
        assert_eq!(items, vec![call]);
    }
    #[test]
    fn store_changes_use_the_latest_registry_snapshot() {
        let source = source();
        let registry = Arc::new(SecretRegistry::default());
        *source.write().unwrap() = Some(registry.clone());
        let change = StoreChange::History {
            session_id: SessionId::new(),
            item: HistoryItem::AssistantText("new-refresh-token".into()),
        };
        let before = changes(std::slice::from_ref(&change), &source).unwrap();
        assert!(
            matches!(&before[0],StoreChange::History{item:HistoryItem::AssistantText(text),..} if text=="new-refresh-token")
        );
        registry.register(&["new-refresh-token".into()]).unwrap();
        let after = changes(&[change], &source).unwrap();
        assert!(
            matches!(&after[0],StoreChange::History{item:HistoryItem::AssistantText(text),..} if text=="[REDACTED]")
        );
        let intent = StoreChange::ToolIntent {
            run_id: RunId::new(),
            call: ToolCall {
                call_id: "new-refresh-token".into(),
                name: "read".into(),
                arguments: "{}".into(),
            },
        };
        assert_eq!(
            changes(&[intent], &source).unwrap_err().0.code,
            "sensitive_data"
        );
    }
}
