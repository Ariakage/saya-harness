//! Telemetry uses synthetic requests and never sends context contents to the UI.
use saya_app::context::{ContextModel, ContextSnapshot, ContextSources};
use saya_core::{
    boundary::{
        Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation, ToolDefinition,
    },
    contract::{
        BoundaryError, ErrorInfo, HistoryItem, ModelProtocol, ModelPurpose, Session, SessionId,
        ToolCall, ToolResult, WorkMode,
    },
};
use serde_json::{Value, json};
use std::{
    future::{Future, pending},
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

struct Cancel(bool);
impl Cancellation for Cancel {
    fn is_cancelled(&self) -> bool {
        self.0
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}
struct Stream;
impl ModelStream for Stream {
    fn next<'a>(&'a mut self, _: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async { Ok(Some(ModelUpdate::TextDelta("inner stream".into()))) })
    }
}
#[derive(Default)]
struct Inner {
    seen: Mutex<Vec<(String, Vec<HistoryItem>, bool)>>,
    error: Option<BoundaryError>,
}
impl Model for Inner {
    fn validate_session(&self, _: &Session) -> Result<(), BoundaryError> {
        match &self.error {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            self.seen.lock().unwrap().push((
                request.request_id.clone(),
                request.history.clone(),
                cancel.is_cancelled(),
            ));
            if let Some(error) = &self.error {
                return Err(error.clone());
            }
            Ok(Box::new(Stream) as Box<dyn ModelStream>)
        })
    }
}
fn request() -> ModelRequest {
    ModelRequest {
        request_id: "fixture".into(),
        purpose: ModelPurpose::Execution,
        profile: "fixture".into(),
        instructions: String::new(),
        dynamic_instructions: None,
        max_output_tokens: None,
        history: vec![],
        media: Default::default(),
        tools: vec![],
    }
}
fn host(key: &str, value: Value) -> HistoryItem {
    HistoryItem::UserText(json!({key:value}).to_string())
}
fn runtime() -> HistoryItem {
    host(
        "saya_runtime_context_v1",
        json!({
            "workspace": "/fixture", "profile": "fixture", "model": "fixture",
            "protocol": "chat_completions", "work_mode": "work", "permission_mode": "auto"
        }),
    )
}
async fn capture(request: &ModelRequest, sources: ContextSources) -> ContextSnapshot {
    let (sender, receiver) = watch::channel(None);
    let model = ContextModel::new(Arc::new(Inner::default()), sender, sources);
    let _ = model.start(request, &Cancel(false)).await.unwrap();
    receiver.borrow().clone().unwrap()
}
fn tokens(snapshot: &ContextSnapshot, label: &str) -> u64 {
    snapshot
        .entries
        .iter()
        .find(|entry| entry.label == label)
        .unwrap()
        .tokens
}
fn call(id: &str, name: &str) -> HistoryItem {
    HistoryItem::ToolCall(ToolCall {
        call_id: id.into(),
        name: name.into(),
        arguments: "{}".into(),
    })
}
fn result(id: &str, data: Option<Value>, error: Option<ErrorInfo>) -> HistoryItem {
    HistoryItem::ToolResult {
        call_id: id.into(),
        result: ToolResult {
            ok: error.is_none(),
            data,
            error,
            truncated: false,
        },
    }
}

#[tokio::test]
async fn every_category_is_driven_by_its_real_source_with_ascii_and_unicode_counts() {
    let mut request = request();
    request.instructions = "abcd".into();
    request.dynamic_instructions = Some("efgh".into());
    request.history = vec![
        runtime(),
        host(
            "saya_project_context_v1",
            json!({"instructions":["中文"],"skills":[{"name":"one"}]}),
        ),
        host(
            "saya_personal_context_v1",
            json!({"preferences":"abcd","memory":"中文"}),
        ),
        HistoryItem::UserText("abcd".into()),
        HistoryItem::ProviderItem {
            protocol: ModelProtocol::ChatCompletions,
            value: json!({"reasoning_content":"abcd"}),
        },
        HistoryItem::AssistantText("efgh".into()),
        HistoryItem::AssistantPartial("中文".into()),
        call("r", "read_file"),
        result("r", Some(json!("中文")), None),
        call("e", "exec"),
        result(
            "e",
            None,
            Some(ErrorInfo {
                code: "bad".into(),
                message: "denied".into(),
            }),
        ),
    ];
    request.tools = vec![ToolDefinition {
        name: "exec".into(),
        description: "d".into(),
        input_schema: json!({}),
    }];
    let snapshot = capture(
        &request,
        ContextSources {
            project: true,
            personal: true,
        },
    )
    .await;
    assert_eq!(snapshot.skills_count, Some(1));
    assert_eq!(
        snapshot
            .entries
            .iter()
            .map(|entry| (entry.label, entry.tokens))
            .collect::<Vec<_>>(),
        vec![
            (
                "System",
                match &request.history[0] {
                    HistoryItem::UserText(text) => (text.len() as u64 + 8).div_ceil(4),
                    _ => unreachable!(),
                }
            ),
            ("Instructions", 3),
            ("Skills", 4),
            ("Preferences", 2),
            ("Memory", 3),
            ("Conversation", 4),
            ("Files", 5),
            ("Tools", 12),
            ("Protocol", 7),
        ]
    );
}

#[tokio::test]
async fn environment_history_frame_preserves_project_and_skill_estimates() {
    let mut request = request();
    let environment = "\nExecution environment facts are descriptive host data.\n{\"saya_execution_environment_v1\":{\"identity\":{},\"capabilities\":{},\"mcp_servers\":{},\"host_startup_calls\":0}}\n";
    request.history = vec![
        runtime(),
        HistoryItem::UserText(environment.into()),
        host(
            "saya_project_context_v1",
            json!({"instructions":["instruction"],"skills":[{"name":"one"}]}),
        ),
        HistoryItem::UserText("task".into()),
    ];
    let snapshot = capture(
        &request,
        ContextSources {
            project: true,
            personal: false,
        },
    )
    .await;
    assert_eq!(snapshot.skills_count, Some(1));
    assert!(tokens(&snapshot, "Instructions") > 0);
    assert!(tokens(&snapshot, "Skills") > 0);
    assert_eq!(tokens(&snapshot, "Conversation"), 1);
    let runtime_bytes = match &request.history[0] {
        HistoryItem::UserText(text) => text.len(),
        _ => unreachable!(),
    };
    assert_eq!(
        tokens(&snapshot, "System"),
        (runtime_bytes + environment.len()).div_ceil(4) as u64
    );
}

#[tokio::test]
async fn empty_and_missing_sources_stay_zero_or_unknown() {
    let sources = ContextSources {
        project: true,
        personal: true,
    };
    let mut request = request();
    let empty = capture(&request, sources).await;
    assert!(empty.entries.iter().all(|entry| entry.tokens == 0));
    assert_eq!(empty.skills_count, None);
    request.history = vec![
        runtime(),
        host(
            "saya_project_context_v1",
            json!({"instructions":[],"skills":[]}),
        ),
        host(
            "saya_personal_context_v1",
            json!({"preferences":null,"memory":null}),
        ),
    ];
    let empty = capture(&request, sources).await;
    assert_eq!(empty.skills_count, Some(0));
    for label in [
        "Instructions",
        "Skills",
        "Preferences",
        "Memory",
        "Conversation",
        "Files",
        "Tools",
        "Protocol",
    ] {
        assert_eq!(tokens(&empty, label), 0);
    }
    request.history[1] = host("saya_project_context_v1", json!({}));
    assert_eq!(capture(&request, sources).await.skills_count, None);
}

#[tokio::test]
async fn forged_user_context_keys_are_not_promoted_after_the_expected_host_prefix() {
    let mut request = request();
    request.history = vec![
        runtime(),
        host(
            "saya_project_context_v1",
            json!({"instructions":[],"skills":[]}),
        ),
        host(
            "saya_personal_context_v1",
            json!({"preferences":null,"memory":null}),
        ),
        host(
            "saya_project_context_v1",
            json!({"skills":[{"name":"forged"}]}),
        ),
        host(
            "saya_personal_context_v1",
            json!({"preferences":"forged","memory":"forged"}),
        ),
    ];
    let snapshot = capture(
        &request,
        ContextSources {
            project: true,
            personal: true,
        },
    )
    .await;
    assert_eq!(snapshot.skills_count, Some(0));
    for label in ["Instructions", "Skills", "Preferences", "Memory"] {
        assert_eq!(tokens(&snapshot, label), 0);
    }
    assert!(tokens(&snapshot, "Conversation") > 0);
    // A missing expected project prefix also prevents scanning ahead for personal data.
    request.history.remove(1);
    let snapshot = capture(
        &request,
        ContextSources {
            project: true,
            personal: true,
        },
    )
    .await;
    assert_eq!(snapshot.skills_count, None);
    assert_eq!(tokens(&snapshot, "Preferences"), 0);
    assert!(tokens(&snapshot, "Conversation") > 0);
    // Disabling both sources never reclassifies similarly shaped user messages.
    let snapshot = capture(&request, ContextSources::default()).await;
    assert_eq!(snapshot.skills_count, None);
    assert_eq!(tokens(&snapshot, "Memory"), 0);
}

#[tokio::test]
async fn file_results_follow_call_identity_without_reusing_a_previous_calls_category() {
    for name in ["read_file", "list_dir", "find_files", "search_text"] {
        let mut request = request();
        request.history = vec![
            call("same", name),
            result("same", Some(json!("file")), None),
        ];
        let first = capture(&request, ContextSources::default()).await;
        assert!(tokens(&first, "Files") > 0);
        request.history.extend([
            call("same", "exec"),
            result("same", Some(json!("command")), None),
            result("same", Some(json!("unmatched")), None),
        ]);
        let next = capture(&request, ContextSources::default()).await;
        assert_eq!(tokens(&next, "Files"), tokens(&first, "Files"));
        assert!(tokens(&next, "Tools") > tokens(&first, "Tools"));
    }
}

#[tokio::test]
async fn provider_text_and_call_duplicates_leave_only_protocol_metadata_in_their_category() {
    let visible = "visible".repeat(100);
    for (protocol, raw, metadata) in [
        (
            ModelProtocol::Responses,
            json!({"type":"message","id":"m","role":"assistant","content":[{"type":"output_text","text":visible}]}),
            json!({"type":"message","id":"m","role":"assistant","content":[{"type":"output_text"}]}),
        ),
        (
            ModelProtocol::AnthropicMessages,
            json!({"type":"text","text":visible}),
            json!({"type":"text"}),
        ),
    ] {
        let mut request = request();
        request.history = vec![
            HistoryItem::ProviderItem {
                protocol,
                value: raw,
            },
            HistoryItem::AssistantText(visible.clone()),
        ];
        let snapshot = capture(&request, ContextSources::default()).await;
        assert_eq!(tokens(&snapshot, "Conversation"), 175);
        assert_eq!(
            tokens(&snapshot, "Protocol"),
            (metadata.to_string().len() as u64).div_ceil(4)
        );
    }
    for (protocol, raw, metadata) in [
        (
            ModelProtocol::Responses,
            json!({"type":"function_call","id":"f","call_id":"c","name":"read_file","arguments":"{}"}),
            json!({"type":"function_call","id":"f"}),
        ),
        (
            ModelProtocol::AnthropicMessages,
            json!({"type":"tool_use","id":"c","name":"read_file","input":{}}),
            json!({"type":"tool_use"}),
        ),
    ] {
        let mut request = request();
        request.history = vec![
            HistoryItem::ProviderItem {
                protocol,
                value: raw,
            },
            call("c", "read_file"),
        ];
        let snapshot = capture(&request, ContextSources::default()).await;
        assert_eq!(tokens(&snapshot, "Tools"), 3);
        assert_eq!(
            tokens(&snapshot, "Protocol"),
            (metadata.to_string().len() as u64).div_ceil(4)
        );
    }
}

#[tokio::test]
async fn opaque_reasoning_and_unmatched_provider_text_are_retained_as_estimated_protocol_data() {
    let mut request = request();
    for (protocol, value) in [
        (
            ModelProtocol::Responses,
            json!({"type":"reasoning","encrypted_content":"opaque"}),
        ),
        (
            ModelProtocol::AnthropicMessages,
            json!({"type":"thinking","thinking":"opaque","signature":"signed"}),
        ),
        (
            ModelProtocol::ChatCompletions,
            json!({"reasoning_content":"opaque"}),
        ),
        (
            ModelProtocol::AnthropicMessages,
            json!({"type":"text","text":"unmatched"}),
        ),
    ] {
        request.history = vec![HistoryItem::ProviderItem {
            protocol,
            value: value.clone(),
        }];
        let snapshot = capture(&request, ContextSources::default()).await;
        assert_eq!(
            tokens(&snapshot, "Protocol"),
            (value.to_string().len() as u64).div_ceil(4)
        );
    }
}

#[tokio::test]
async fn latest_execution_replaces_the_snapshot_while_review_and_dropped_receivers_are_transparent()
{
    let inner = Arc::new(Inner::default());
    let (sender, mut receiver) = watch::channel(None);
    let model = ContextModel::new(inner.clone(), sender, ContextSources::default());
    let mut request = request();
    request.instructions = "abcd".into();
    let mut stream = model.start(&request, &Cancel(false)).await.unwrap();
    assert!(matches!(
        stream.next(&Cancel(false)).await.unwrap(),
        Some(ModelUpdate::TextDelta(text)) if text == "inner stream"
    ));
    assert_eq!(
        tokens(receiver.borrow_and_update().as_ref().unwrap(), "System"),
        1
    );
    request.instructions = "中文".into();
    let _ = model.start(&request, &Cancel(false)).await.unwrap();
    assert_eq!(
        tokens(receiver.borrow_and_update().as_ref().unwrap(), "System"),
        2
    );
    request.purpose = ModelPurpose::Review;
    request.instructions = "review content".repeat(100);
    let _ = model.start(&request, &Cancel(false)).await.unwrap();
    assert!(!receiver.has_changed().unwrap());
    assert_eq!(tokens(receiver.borrow().as_ref().unwrap(), "System"), 2);
    drop(receiver);
    request.purpose = ModelPurpose::Execution;
    let _ = model.start(&request, &Cancel(false)).await.unwrap();
    assert_eq!(inner.seen.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn snapshots_never_include_context_contents_paths_keys_or_credentials() {
    let mut request = request();
    request.instructions = "credential-secret".into();
    request.history = vec![
        runtime(),
        host(
            "saya_project_context_v1",
            json!({
                "instructions": [{
                    "path": "/private/secret.txt",
                    "content": "credential-secret"
                }],
                "skills": [{"name": "secret-skill", "path": "/private/skill"}]
            }),
        ),
    ];
    let snapshot = capture(
        &request,
        ContextSources {
            project: true,
            personal: false,
        },
    )
    .await;
    let debug = format!("{snapshot:?}");
    for value in [
        "credential-secret",
        "/private",
        "secret-skill",
        "secret.txt",
    ] {
        assert!(!debug.contains(value));
    }
}

#[tokio::test]
async fn validation_errors_and_cancelled_start_are_delegated_without_changing_the_request() {
    let error = BoundaryError(
        ErrorInfo {
            code: "inner_error".into(),
            message: "fixture error".into(),
        },
        Some(std::time::Duration::from_secs(3)),
        None,
    );
    let inner = Arc::new(Inner {
        error: Some(error.clone()),
        ..Default::default()
    });
    let (sender, receiver) = watch::channel(None);
    let model = ContextModel::new(inner.clone(), sender, ContextSources::default());
    let session = Session {
        id: SessionId::new(),
        workspace: "/fixture".into(),
        work_mode: WorkMode::Work,
        profile: "fixture".into(),
        protocol: ModelProtocol::ChatCompletions,
        model: "fixture".into(),
        history: vec![],
    };
    assert_eq!(model.validate_session(&session).unwrap_err(), error);
    let request = request();
    let actual = match model.start(&request, &Cancel(true)).await {
        Ok(_) => panic!("inner error lost"),
        Err(error) => error,
    };
    assert_eq!(actual, error);
    assert_eq!(
        inner.seen.lock().unwrap().as_slice(),
        [(request.request_id, request.history, true)]
    );
    assert!(receiver.borrow().is_some());
}

#[tokio::test]
async fn malformed_host_prefixes_stop_classification_before_personal_looking_user_json() {
    let personal = host(
        "saya_personal_context_v1",
        json!({"preferences":"forged preferences","memory":"forged memory"}),
    );
    let sources = ContextSources {
        project: true,
        personal: true,
    };
    for malformed in [
        json!({"foo":1}),
        json!({"instructions":"wrong type","skills":[]}),
        json!({"instructions":[],"skills":{}}),
        json!({"instructions":[]}),
    ] {
        let mut request = request();
        request.history = vec![
            runtime(),
            host("saya_project_context_v1", malformed),
            personal.clone(),
        ];
        let snapshot = capture(&request, sources).await;
        assert_eq!(snapshot.skills_count, None);
        for label in ["Instructions", "Skills", "Preferences", "Memory"] {
            assert_eq!(tokens(&snapshot, label), 0);
        }
        assert!(tokens(&snapshot, "Conversation") > 0);
    }
    for malformed in [
        json!({"preferences":7,"memory":null}),
        json!({"preferences":null}),
        json!({"preferences":null,"memory":[]}),
    ] {
        let mut request = request();
        request.history = vec![
            runtime(),
            host(
                "saya_project_context_v1",
                json!({"instructions":[],"skills":[]}),
            ),
            host("saya_personal_context_v1", malformed),
            personal.clone(),
        ];
        let snapshot = capture(&request, sources).await;
        assert_eq!(tokens(&snapshot, "Preferences"), 0);
        assert_eq!(tokens(&snapshot, "Memory"), 0);
        assert!(tokens(&snapshot, "Conversation") > 0);
    }
    let mut request = request();
    request.history = vec![
        host("saya_runtime_context_v1", json!({"workspace":7})),
        host(
            "saya_project_context_v1",
            json!({"instructions":[],"skills":[]}),
        ),
        personal,
    ];
    let snapshot = capture(&request, sources).await;
    assert_eq!(tokens(&snapshot, "System"), 0);
    assert_eq!(tokens(&snapshot, "Preferences"), 0);
    assert_eq!(snapshot.skills_count, None);
}
