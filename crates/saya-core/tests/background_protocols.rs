#[path = "fixtures/http_support/mod.rs"]
mod fixture;
mod support;
use saya_core::{
    approvals::Preauthorizations,
    background::{BackgroundCancelReason, BackgroundEventKind, BackgroundOutcome},
    boundary::{Cancellation, Model, Store},
    contract::{
        HistoryItem, ModelProtocol, PermissionMode, Run, RunId, RunStatus, Session, SessionId,
        WorkMode,
    },
    memory::MemoryStore,
    permissions::{PermissionConfig, Policy},
    provider::{
        ModelSettings,
        anthropic::{AnthropicModel, AnthropicVersion},
        chat::{ChatCompatibility, ChatModel},
        responses::{ResponsesCompatibility, ResponsesModel},
        transport::{Authentication, HttpConfig, HttpTransport},
    },
    review::ReviewConfig,
    runtime::{AgentRuntime, RuntimeLimits, RuntimeSetup},
    tools::{
        exec::ExecConfig,
        read::{ReadLimits, ReadTools},
    },
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::{Future, pending},
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
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
fn adapter(
    protocol: ModelProtocol,
    url: String,
) -> Result<Arc<dyn Model>, Box<dyn std::error::Error>> {
    let http = Arc::new(HttpTransport::new(HttpConfig {
        base_url: url,
        allow_remote_http: false,
        authentication: Authentication::None,
        headers_env: BTreeMap::new(),
        connect_timeout: Duration::from_secs(2),
        idle_timeout: Duration::from_secs(4),
        total_timeout: Duration::from_secs(8),
    })?);
    let settings = ModelSettings {
        profile: "test".into(),
        model: "fixture".into(),
        context_window_tokens: 128_000,
        extra_safety_tokens: 0,
        max_output_tokens: 8192,
        upstream_max_output_tokens: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        thinking_effort: None,
        strict_tools: false,
        stream: true,
    };
    Ok(match protocol {
        ModelProtocol::ChatCompletions => {
            Arc::new(ChatModel::new(http, settings, ChatCompatibility::default()))
        }
        ModelProtocol::Responses => Arc::new(ResponsesModel::new(
            http,
            settings,
            ResponsesCompatibility::default(),
        )),
        ModelProtocol::AnthropicMessages => Arc::new(AnthropicModel::new(
            http,
            settings,
            AnthropicVersion::default(),
        )),
    })
}
fn event(kind: &str, mut data: Value) -> Value {
    data["type"] = json!(kind);
    data
}
fn response(protocol: ModelProtocol, call: Option<(&str, &str, Value)>, text: &str) -> String {
    let has_call = call.is_some();
    match protocol {
        ModelProtocol::ChatCompletions => {
            let delta = match call {
                Some((id, name, args)) => {
                    json!({"tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]})
                }
                None => json!({"content":text}),
            };
            fixture::sse(&[
                json!({"choices":[{"index":0,"delta":delta,"finish_reason":null}]}),
                json!({"choices":[{"index":0,"delta":{},"finish_reason":if has_call{"tool_calls"}else{"stop"}}]}),
            ])
        }
        ModelProtocol::Responses => {
            let item = match call {
                Some((id, name, args)) => {
                    json!({"type":"function_call","id":format!("item-{id}"),"status":"completed","call_id":id,"name":name,"arguments":args.to_string()})
                }
                None => {
                    json!({"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":text}]})
                }
            };
            fixture::sse(&[
                event(
                    "response.output_item.done",
                    json!({"output_index":0,"item":item}),
                ),
                event(
                    "response.completed",
                    json!({"response":{"status":"completed","output":[item]}}),
                ),
            ])
        }
        ModelProtocol::AnthropicMessages => {
            let block = match call {
                Some((id, name, args)) => {
                    json!({"type":"tool_use","id":id,"name":name,"input":args})
                }
                None => json!({"type":"text","text":text}),
            };
            let events = [
                event(
                    "message_start",
                    json!({"message":{"role":"assistant","content":[]}}),
                ),
                event(
                    "content_block_start",
                    json!({"index":0,"content_block":block}),
                ),
                event("content_block_stop", json!({"index":0})),
                event(
                    "message_delta",
                    json!({"delta":{"stop_reason":if has_call{"tool_use"}else{"end_turn"}}}),
                ),
                event("message_stop", json!({})),
            ];
            events
                .iter()
                .map(|e| {
                    format!(
                        "event: {}\ndata: {}\n\n",
                        e["type"].as_str().unwrap_or(""),
                        e
                    )
                })
                .collect()
        }
    }
}
fn tool_result(protocol: ModelProtocol, body: &Value, id: &str) -> Option<Value> {
    let raw = match protocol {
        ModelProtocol::ChatCompletions => body["messages"]
            .as_array()?
            .iter()
            .find(|m| m["role"] == "tool" && m["tool_call_id"] == id)?
            .get("content")?
            .as_str()?,
        ModelProtocol::Responses => body["input"]
            .as_array()?
            .iter()
            .find(|m| m["type"] == "function_call_output" && m["call_id"] == id)?
            .get("output")?
            .as_str()?,
        ModelProtocol::AnthropicMessages => body["messages"]
            .as_array()?
            .iter()
            .filter_map(|m| m["content"].as_array())
            .flatten()
            .find(|m| m["type"] == "tool_result" && m["tool_use_id"] == id)?
            .get("content")?
            .as_str()?,
    };
    serde_json::from_str(raw).ok()
}
fn build_runtime(
    protocol: ModelProtocol,
    root: &std::path::Path,
    model: Arc<dyn Model>,
    permissions: PermissionMode,
    model_requests: usize,
    tool_calls: usize,
) -> Result<(AgentRuntime, Session, Arc<MemoryStore>), Box<dyn std::error::Error>> {
    let session = Session {
        id: SessionId::new(),
        workspace: root.into(),
        work_mode: WorkMode::Work,
        profile: "test".into(),
        protocol,
        model: "fixture".into(),
        history: vec![HistoryItem::UserText("run a background command".into())],
    };
    let run = Run {
        id: RunId::new(),
        session_id: session.id,
        work_mode: WorkMode::Work,
        permissions,
    };
    let store = Arc::new(MemoryStore::default());
    let limits = RuntimeLimits {
        model_requests,
        tool_calls,
        ..RuntimeLimits::default()
    };
    let runtime = AgentRuntime::new(RuntimeSetup {
        run,
        model,
        store: store.clone(),
        policy: Arc::new(Policy::new(root, PermissionConfig::default())?),
        files: Arc::new(ReadTools::new(
            root,
            &[],
            permissions == PermissionMode::Bypass,
            ReadLimits::default(),
        )?),
        cancellation: Arc::new(Never),
        review: ReviewConfig::new("test".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "run background command".into(),
        confirmation: None,
        limits,
    })?;
    Ok((runtime, session, store))
}
#[tokio::test]
async fn all_protocols_continue_while_job_runs_and_cleanup_is_observable()
-> Result<(), Box<dyn std::error::Error>> {
    for protocol in [
        ModelProtocol::ChatCompletions,
        ModelProtocol::Responses,
        ModelProtocol::AnthropicMessages,
    ] {
        for (limit, tool_limit) in [(64usize, 256usize), (2, 256), (64, 1)] {
            let temp = support::temp_workspace()?;
            let marker = temp.path().join("started");
            let target = marker.clone();
            let (url, requests) = fixture::server_dynamic(
                if limit == 2 || tool_limit == 1 { 2 } else { 3 },
                move |index, body| {
                    let stream = match index {
                        0 => response(
                            protocol,
                            Some((
                                "launch",
                                "background_start",
                                json!({"tool":"exec","arguments":{"argv":["/bin/sh","-c","printf ready > \"$1\"; sleep 30","sh",target]}}),
                            )),
                            "",
                        ),
                        1 => {
                            let until = Instant::now() + Duration::from_secs(3);
                            while !target.exists() && Instant::now() < until {
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            let id = tool_result(protocol, body, "launch")
                                .and_then(|v| v["data"]["job_id"].as_str().map(str::to_owned))
                                .unwrap_or_else(|| "missing".into());
                            response(
                                protocol,
                                Some(("status", "background_status", json!({"job_id":id}))),
                                "",
                            )
                        }
                        _ => response(protocol, None, "done"),
                    };
                    ("text/event-stream", stream)
                },
            )?;
            let (mut runtime, mut session, store) = build_runtime(
                protocol,
                temp.path(),
                adapter(protocol, url)?,
                PermissionMode::Bypass,
                limit,
                tool_limit,
            )?;
            let handle = runtime.handle();
            let result =
                tokio::time::timeout(Duration::from_secs(15), runtime.run(&mut session)).await??;
            assert_eq!(
                result.status,
                if limit == 2 || tool_limit == 1 {
                    RunStatus::LimitReached
                } else {
                    RunStatus::Completed
                },
                "{protocol:?}"
            );
            assert!(
                marker.exists(),
                "model did not overlap a running job for {protocol:?}"
            );
            let _first = requests.recv_timeout(Duration::from_secs(1))?;
            let second = requests.recv_timeout(Duration::from_secs(1))?;
            let launch = tool_result(protocol, &second, "launch").ok_or("launch result missing")?;
            let job_id = launch["data"]["job_id"].as_str().ok_or("job ID missing")?;
            if limit != 2 && tool_limit != 1 {
                let third = requests.recv_timeout(Duration::from_secs(1))?;
                let status =
                    tool_result(protocol, &third, "status").ok_or("status result missing")?;
                assert_eq!(status["data"]["status"], "running");
            }
            if tool_limit == 1 {
                assert!(session.history.iter().any(|item| matches!(item, HistoryItem::ToolResult { call_id, result } if call_id == "status" && result.error.as_ref().is_some_and(|e| e.code == "not_executed"))));
            }
            let events = handle.background_events()?;
            assert!(events.len() >= 3, "{events:?}");
            assert!(
                events
                    .windows(2)
                    .all(|pair| pair[1].sequence == pair[0].sequence + 1)
            );
            let job_events: Vec<_> = events.iter().filter(|e| e.job_id == job_id).collect();
            assert!(
                matches!(job_events.first().map(|e|&e.kind),Some(BackgroundEventKind::Accepted{tool,..}) if tool=="exec")
            );
            assert!(job_events.iter().any(|e| matches!(
                e.kind,
                BackgroundEventKind::CancellationRequested {
                    reason: BackgroundCancelReason::Shutdown
                }
            )));
            assert!(
                matches!(
                    job_events.last().map(|e| &e.kind),
                    Some(BackgroundEventKind::Finished {
                        outcome: BackgroundOutcome::Cancelled
                    })
                ),
                "{job_events:?}"
            );
            assert_eq!(
                store
                    .load_session(session.id)
                    .await?
                    .ok_or("session missing")?
                    .history,
                session.history
            );
        }
    }
    Ok(())
}
#[tokio::test]
async fn background_review_and_execution_share_model_budget_across_protocols()
-> Result<(), Box<dyn std::error::Error>> {
    for protocol in [
        ModelProtocol::ChatCompletions,
        ModelProtocol::Responses,
        ModelProtocol::AnthropicMessages,
    ] {
        let temp = support::temp_workspace()?;
        let (url, requests) = fixture::server_dynamic(2, move |index, _| {
            let body = if index == 0 {
                response(
                    protocol,
                    Some((
                        "launch",
                        "background_start",
                        json!({"tool":"exec","arguments":{"argv":["/bin/sleep","30"]}}),
                    )),
                    "",
                )
            } else {
                response(
                    protocol,
                    None,
                    r#"{"decision":"allow","reason":"bounded test"}"#,
                )
            };
            ("text/event-stream", body)
        })?;
        let (mut runtime, mut session, _) = build_runtime(
            protocol,
            temp.path(),
            adapter(protocol, url)?,
            PermissionMode::Auto,
            2,
            256,
        )?;
        let result =
            tokio::time::timeout(Duration::from_secs(15), runtime.run(&mut session)).await??;
        assert_eq!(result.status, RunStatus::LimitReached, "{protocol:?}");
        let _ = requests.recv_timeout(Duration::from_secs(1))?;
        let review = requests.recv_timeout(Duration::from_secs(1))?;
        assert!(
            review.get("tools").is_none(),
            "review exposed tools for {protocol:?}"
        );
        assert!(session.history.iter().any(|i|matches!(i,HistoryItem::ToolResult{call_id,result} if call_id=="launch" && result.ok)));
    }
    Ok(())
}

#[tokio::test]
async fn recovered_tool_pairs_remain_valid_for_all_three_provider_protocols()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::{
        boundary::{ModelRequest, ModelUpdate, StoreChange},
        contract::{ModelPurpose, ToolCall},
        sqlite::SqliteStore,
    };
    for protocol in [
        ModelProtocol::ChatCompletions,
        ModelProtocol::Responses,
        ModelProtocol::AnthropicMessages,
    ] {
        let temp = support::temp_workspace()?;
        let session = Session {
            id: SessionId::new(),
            workspace: temp.path().into(),
            work_mode: WorkMode::Work,
            profile: "test".into(),
            protocol,
            model: "fixture".into(),
            history: vec![HistoryItem::UserText("first".into())],
        };
        let run = Run {
            id: RunId::new(),
            session_id: session.id,
            work_mode: session.work_mode,
            permissions: PermissionMode::Auto,
        };
        let call = ToolCall {
            call_id: "crash-call".into(),
            name: "read_file".into(),
            arguments: "{\"path\":\"a\"}".into(),
        };
        let store = SqliteStore::open(&temp.path().join("data"), vec![])?;
        store.create_session(&session, "hash").await?;
        let mut changes = vec![StoreChange::RunStarted(run.clone())];
        match protocol {
            ModelProtocol::ChatCompletions=>{},
            ModelProtocol::Responses=>changes.push(StoreChange::History {session_id:session.id,item:HistoryItem::ProviderItem {protocol,value:json!({"type":"function_call","id":"fc_1","status":"completed","call_id":"crash-call","name":"read_file","arguments":"{\"path\":\"a\"}"})}}),
            ModelProtocol::AnthropicMessages=>{
                changes.push(StoreChange::History {session_id:session.id,item:HistoryItem::ProviderItem {protocol,value:json!({"type":"thinking","thinking":"opaque","signature":"signed-fixture"})}});
                changes.push(StoreChange::History {session_id:session.id,item:HistoryItem::ProviderItem {protocol,value:json!({"type":"tool_use","id":"crash-call","name":"read_file","input":{"path":"a"}})}});
            }
        }
        changes.push(StoreChange::History {
            session_id: session.id,
            item: HistoryItem::ToolCall(call.clone()),
        });
        changes.push(StoreChange::ToolIntent {
            run_id: run.id,
            call,
        });
        store.commit(&changes).await?;
        let _lock = store.lock_session(session.id)?;
        assert_eq!(store.recover_interrupted(session.id).await?.closed_calls, 1);
        let mut recovered = store.load_session(session.id).await?.ok_or("missing")?;
        recovered
            .history
            .push(HistoryItem::UserText("continue".into()));
        let (url, requests) =
            fixture::server(vec![("text/event-stream", response(protocol, None, "ok"))])?;
        let model = adapter(protocol, url)?;
        let req = ModelRequest {
            request_id: "id".into(),
            purpose: ModelPurpose::Execution,
            profile: "test".into(),
            instructions: "test".into(),
            dynamic_instructions: None,
            max_output_tokens: None,
            history: recovered.history,
            tools: vec![],
        };
        let mut stream = model.start(&req, &Never).await?;
        loop {
            match stream.next(&Never).await? {
                Some(ModelUpdate::Finished { .. }) => break,
                Some(_) => {}
                None => return Err("missing result".into()),
            }
        }
        let sent = requests.recv_timeout(Duration::from_secs(2))?.to_string();
        assert!(sent.contains("crash-call"));
        assert!(sent.contains("outcome_unknown"));
        if protocol == ModelProtocol::AnthropicMessages {
            assert!(sent.contains("signed-fixture"));
        }
    }
    Ok(())
}
