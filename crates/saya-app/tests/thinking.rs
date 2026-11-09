use saya_app::thinking::{ThinkingLog, ThinkingModel, extract_reasoning};
use saya_core::{
    boundary::{Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation},
    contract::{BoundaryError, ErrorInfo, HistoryItem, ModelProtocol, ModelPurpose},
};
use serde_json::json;
use std::{collections::VecDeque, future::pending, pin::Pin, sync::Arc, time::Duration};
use tokio::sync::watch;

struct Cancel(bool);
impl Cancellation for Cancel {
    fn is_cancelled(&self) -> bool {
        self.0
    }
    fn cancelled(&self) -> Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}
fn error() -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "fixture_error".into(),
            message: "original failure".into(),
        },
        None,
        None,
    )
}
struct Fake;
impl Model for Fake {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async {
            Ok(Box::new(Stream(VecDeque::from([Ok(finished(vec![native(
                ModelProtocol::ChatCompletions,
                json!({"reasoning_content":"retained thought"}),
            )]))]))) as Box<dyn ModelStream>)
        })
    }
}
struct Stream(VecDeque<Result<ModelUpdate, BoundaryError>>);
impl ModelStream for Stream {
    fn next<'a>(&'a mut self, cancel: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(error());
            }
            self.0.pop_front().transpose()
        })
    }
}
struct Fixed(Vec<Result<ModelUpdate, BoundaryError>>);
impl Model for Fixed {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async { Ok(Box::new(Stream(self.0.clone().into())) as Box<dyn ModelStream>) })
    }
}
fn request(id: &str) -> ModelRequest {
    ModelRequest {
        request_id: id.into(),
        purpose: ModelPurpose::Execution,
        profile: "fixture".into(),
        instructions: String::new(),
        dynamic_instructions: None,
        max_output_tokens: None,
        history: vec![],
        tools: vec![],
    }
}
fn thought(text: impl Into<String>) -> ModelUpdate {
    ModelUpdate::ThinkingDelta { text: text.into() }
}
fn native(protocol: ModelProtocol, value: serde_json::Value) -> HistoryItem {
    HistoryItem::ProviderItem { protocol, value }
}
fn finished(items: Vec<HistoryItem>) -> ModelUpdate {
    ModelUpdate::Finished { items, usage: None }
}
fn observe(
    updates: Vec<Result<ModelUpdate, BoundaryError>>,
    secrets: Vec<String>,
) -> (ThinkingModel, watch::Receiver<ThinkingLog>) {
    let (sender, receiver) = watch::channel(ThinkingLog::default());
    (
        ThinkingModel::new(Arc::new(Fixed(updates)), sender, secrets),
        receiver,
    )
}

struct Starting(bool);
impl Model for Starting {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        if self.0 {
            Box::pin(async { Err(error()) })
        } else {
            Box::pin(pending())
        }
    }
}

#[tokio::test]
async fn start_failure_and_dropped_network_wait_close_the_active_placeholder() {
    let (sender, mut log) = watch::channel(ThinkingLog::default());
    let model = ThinkingModel::new(Arc::new(Starting(false)), sender, vec![]);
    let request = request("network");
    let cancel = Cancel(false);
    let mut start = model.start(&request, &cancel);
    tokio::select! {
        _ = &mut start => panic!("fixture network wait completed"),
        changed = log.changed() => { changed.unwrap(); }
    }
    assert!(log.borrow().entries[0].active);
    drop(start);
    assert!(!log.borrow().entries[0].active);
    let (sender, log) = watch::channel(ThinkingLog::default());
    let model = ThinkingModel::new(Arc::new(Starting(true)), sender, vec![]);
    let returned = model.start(&request, &cancel).await.err().unwrap();
    assert_eq!(returned.0, error().0);
    assert!(!log.borrow().entries[0].active);
}

#[tokio::test]
async fn redaction_failure_withholds_text_without_changing_native_completion() {
    let secret = "fixture\"secret\\tail".to_owned();
    let payload = r#"{"token":"fixture\u0022secret\u005ctail","n":1e400}"#;
    let items = vec![native(
        ModelProtocol::ChatCompletions,
        json!({"reasoning_content":payload}),
    )];
    let (model, log) = observe(vec![Ok(finished(items.clone()))], vec![secret]);
    let request = request("withheld");
    let mut stream = model.start(&request, &Cancel(false)).await.unwrap();
    assert!(matches!(stream.next(&Cancel(false)).await.unwrap(),
        Some(ModelUpdate::Finished { items: returned, .. }) if returned == items));
    let snapshot = &log.borrow().entries[0];
    assert!(
        snapshot.text.is_empty() && snapshot.truncated && snapshot.available && !snapshot.active
    );
}

#[test]
fn extraction_allows_only_known_plaintext_fields() {
    let items = vec![
        HistoryItem::AssistantText("visible answer".into()),
        native(
            ModelProtocol::ChatCompletions,
            json!({"reasoning_content":"chat","signature":"opaque"}),
        ),
        native(
            ModelProtocol::AnthropicMessages,
            json!({"type":"thinking","thinking":"anthropic","signature":"opaque"}),
        ),
        native(
            ModelProtocol::AnthropicMessages,
            json!({"type":"redacted_thinking","data":"hidden","thinking":"forged"}),
        ),
        native(
            ModelProtocol::Responses,
            json!({"type":"reasoning","encrypted_content":"hidden",
            "summary":[{"type":"summary_text","text":"summary"},{"type":"encrypted","text":"forged"}],
            "content":[{"type":"reasoning_text","text":"public"}]}),
        ),
    ];
    assert_eq!(
        extract_reasoning(&items),
        "chat\nanthropic\nsummary\npublic"
    );
}

#[test]
fn oversized_restored_plaintext_has_a_visible_notice_without_projecting_opaque_data() {
    let oversized = "界".repeat(16 * 1024 * 1024 / 3 + 1);
    for item in [
        native(
            ModelProtocol::ChatCompletions,
            json!({"reasoning_content":oversized}),
        ),
        native(
            ModelProtocol::AnthropicMessages,
            json!({"type":"thinking","thinking":oversized,"signature":"opaque"}),
        ),
        native(
            ModelProtocol::Responses,
            json!({"type":"reasoning","summary":[{"type":"summary_text","text":oversized}],"encrypted_content":"opaque"}),
        ),
    ] {
        let items = vec![item];
        let original = items.clone();
        let displayed = extract_reasoning(&items);
        assert!(
            displayed.contains("truncated") && displayed.contains("16 MiB"),
            "{displayed:?}"
        );
        assert!(!displayed.contains('界'));
        assert!(!displayed.contains("opaque"));
        assert_eq!(items, original);
    }
    let only_opaque = vec![
        native(
            ModelProtocol::AnthropicMessages,
            json!({"type":"redacted_thinking","data":oversized,"thinking":"forged"}),
        ),
        native(
            ModelProtocol::Responses,
            json!({"type":"reasoning","encrypted_content":oversized}),
        ),
    ];
    assert!(extract_reasoning(&only_opaque).is_empty());
}

#[test]
fn recovery_limit_preserves_complete_text_and_counts_joined_plaintext_fields() {
    let complete = "x".repeat(16 * 1024 * 1024);
    let within = vec![native(
        ModelProtocol::ChatCompletions,
        json!({"reasoning_content":complete}),
    )];
    assert_eq!(extract_reasoning(&within), complete);
    let mut joined = within;
    joined.push(native(
        ModelProtocol::Responses,
        json!({"type":"reasoning","summary":[{"type":"summary_text","text":"界"}]}),
    ));
    let displayed = extract_reasoning(&joined);
    assert!(displayed.contains("truncated") && displayed.len() < 256);
    let opaque_only = vec![native(
        ModelProtocol::Responses,
        json!({"type":"reasoning","encrypted_content":complete,
        "summary":[{"type":"summary_text","text":"可读 🌸"},{"type":"encrypted","text":"forged"}]}),
    )];
    assert_eq!(extract_reasoning(&opaque_only), "可读 🌸");
}

#[tokio::test]
async fn thinking_is_progress_and_completed_json_uses_native_plaintext() {
    let items = vec![
        native(
            ModelProtocol::ChatCompletions,
            json!({"reasoning_content":"complete thought"}),
        ),
        HistoryItem::AssistantText("answer".into()),
    ];
    let (model, log) = observe(
        vec![
            Ok(thought("partial thought")),
            Ok(ModelUpdate::TextDelta("answer".into())),
            Ok(finished(items.clone())),
        ],
        vec![],
    );
    let request = request("one");
    let mut stream = model.start(&request, &Cancel(false)).await.unwrap();
    assert!(log.borrow().entries[0].active);
    assert!(!log.borrow().entries[0].available);
    assert!(matches!(
        stream.next(&Cancel(false)).await.unwrap(),
        Some(ModelUpdate::Progress)
    ));
    assert!(
        matches!(stream.next(&Cancel(false)).await.unwrap(), Some(ModelUpdate::TextDelta(text)) if text=="answer")
    );
    assert!(
        matches!(stream.next(&Cancel(false)).await.unwrap(), Some(ModelUpdate::Finished { items: returned, .. }) if returned==items)
    );
    let snapshot = &log.borrow().entries[0];
    assert_eq!(snapshot.text, "complete thought");
    assert!(snapshot.available && !snapshot.active && !snapshot.truncated);
}

#[tokio::test]
async fn known_secrets_are_redacted_before_the_display_boundary_and_never_published_live() {
    let secret = "fixture-secret-tail";
    let prefix = "x".repeat(256 * 1024 - 5);
    let (model, mut log) = observe(
        vec![
            Ok(thought(format!("{prefix}fixture-"))),
            Ok(thought("secret-tail after")),
            Ok(finished(vec![HistoryItem::AssistantText("answer".into())])),
        ],
        vec![secret.into()],
    );
    let request = request("secret");
    let mut stream = model.start(&request, &Cancel(false)).await.unwrap();
    log.borrow_and_update();
    stream.next(&Cancel(false)).await.unwrap();
    assert!(log.borrow().entries[0].text.is_empty());
    tokio::time::sleep(Duration::from_millis(35)).await;
    stream.next(&Cancel(false)).await.unwrap();
    assert!(log.has_changed().unwrap());
    assert!(log.borrow().entries[0].text.is_empty());
    stream.next(&Cancel(false)).await.unwrap();
    let snapshot = &log.borrow().entries[0];
    assert!(snapshot.text.ends_with("[REDA"));
    assert!(!snapshot.text.contains(secret));
    assert!(snapshot.truncated && !snapshot.active);
}

#[tokio::test]
async fn errors_cancellation_and_drop_finish_entries_without_changing_errors() {
    for cancel in [false, true] {
        let (model, log) = observe(vec![Ok(thought("safe thought")), Err(error())], vec![]);
        let request = request("error");
        let mut stream = model.start(&request, &Cancel(false)).await.unwrap();
        stream.next(&Cancel(false)).await.unwrap();
        let returned = stream.next(&Cancel(cancel)).await.unwrap_err();
        assert_eq!(returned.0, error().0);
        assert_eq!(log.borrow().entries[0].text, "safe thought");
        assert!(!log.borrow().entries[0].active);
    }
    let (model, log) = observe(vec![Ok(thought("safe thought"))], vec![]);
    let request = request("drop");
    let mut stream = model.start(&request, &Cancel(false)).await.unwrap();
    stream.next(&Cancel(false)).await.unwrap();
    drop(stream);
    assert!(!log.borrow().entries[0].active);
    assert_eq!(log.borrow().entries[0].text, "safe thought");
}

#[tokio::test]
async fn fast_requests_retain_a_bounded_history_and_review_is_private() {
    let (sender, log) = watch::channel(ThinkingLog::default());
    let model = ThinkingModel::new(Arc::new(Fake), sender, vec![]);
    for index in 0..70 {
        let request = request(&format!("request-{index}"));
        let mut stream = model.start(&request, &Cancel(false)).await.unwrap();
        stream.next(&Cancel(false)).await.unwrap();
    }
    assert_eq!(log.borrow().entries.len(), 64);
    assert_eq!(log.borrow().entries[0].request_id, "request-6");
    assert!(
        log.borrow()
            .entries
            .iter()
            .all(|entry| !entry.active && entry.text == "retained thought")
    );
    let (model, log) = observe(
        vec![Ok(thought("private review")), Ok(finished(vec![]))],
        vec![],
    );
    let mut request = request("review");
    request.purpose = ModelPurpose::Review;
    let mut stream = model.start(&request, &Cancel(false)).await.unwrap();
    assert!(matches!(
        stream.next(&Cancel(false)).await.unwrap(),
        Some(ModelUpdate::Progress)
    ));
    stream.next(&Cancel(false)).await.unwrap();
    assert!(log.borrow().entries.is_empty());
}

#[tokio::test]
async fn log_and_entry_limits_are_utf8_safe_and_raw_overflow_does_not_fail_execution() {
    let text = "界".repeat(100_000);
    let (model, log) = observe(
        vec![Ok(finished(vec![native(
            ModelProtocol::ChatCompletions,
            json!({"reasoning_content":text}),
        )]))],
        vec![],
    );
    for index in 0..10 {
        let request = request(&format!("large-{index}"));
        model
            .start(&request, &Cancel(false))
            .await
            .unwrap()
            .next(&Cancel(false))
            .await
            .unwrap();
    }
    let log = log.borrow();
    assert_eq!(log.entries.len(), 8);
    assert_eq!(log.entries[0].request_id, "large-2");
    assert!(
        log.entries
            .iter()
            .all(|entry| entry.text.len() <= 256 * 1024 && entry.truncated && !entry.active)
    );
    assert!(
        log.entries
            .iter()
            .map(|entry| entry.text.len())
            .sum::<usize>()
            <= 2 * 1024 * 1024
    );
    drop(log);
    let (model, log) = observe(
        vec![
            Ok(thought("x".repeat(16 * 1024 * 1024))),
            Ok(thought("overflow")),
            Ok(finished(vec![HistoryItem::AssistantText("answer".into())])),
        ],
        vec![],
    );
    let request = request("overflow");
    let mut stream = model.start(&request, &Cancel(false)).await.unwrap();
    stream.next(&Cancel(false)).await.unwrap();
    stream.next(&Cancel(false)).await.unwrap();
    assert!(matches!(
        stream.next(&Cancel(false)).await.unwrap(),
        Some(ModelUpdate::Finished { .. })
    ));
    let snapshot = &log.borrow().entries[0];
    assert!(
        snapshot.text.is_empty() && snapshot.truncated && snapshot.available && !snapshot.active
    );
}

#[tokio::test]
async fn rotating_secrets_are_observed_after_capture_begins_and_never_stream_live() {
    let registry = Arc::new(saya_core::credentials::SecretRegistry::default());
    let (model, log) = observe(
        vec![
            Ok(thought("rotation-")),
            Ok(thought("synthetic-value")),
            Ok(finished(vec![HistoryItem::AssistantText("answer".into())])),
        ],
        vec!["static-synthetic-secret".into()],
    );
    let model = model.with_secret_registry(registry.clone());
    let request = request("rotation");
    let mut stream = model.start(&request, &Cancel(false)).await.unwrap();
    assert!(
        registry
            .snapshot()
            .unwrap()
            .contains(&"static-synthetic-secret".into())
    );
    tokio::time::sleep(Duration::from_millis(35)).await;
    stream.next(&Cancel(false)).await.unwrap();
    assert!(log.borrow().entries[0].text.is_empty());
    registry
        .register(&["rotation-synthetic-value".into()])
        .unwrap();
    tokio::time::sleep(Duration::from_millis(35)).await;
    stream.next(&Cancel(false)).await.unwrap();
    assert!(log.borrow().entries[0].text.is_empty());
    stream.next(&Cancel(false)).await.unwrap();
    let snapshot = &log.borrow().entries[0];
    assert_eq!(snapshot.text, "[REDACTED]");
    assert!(!snapshot.active);
}
