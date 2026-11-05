use super::*;
use crate::boundary::{ModelStream, Operation, ToolDefinition};
use crate::contract::{ModelProtocol, PermissionMode, RunId, SessionId, WorkMode};
use crate::memory::MemoryStore;
use serde_json::json;
use std::collections::VecDeque;
use std::future::{Future, pending};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

struct NoCancel;
impl Cancellation for NoCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}
struct FakeModel {
    turns: Mutex<VecDeque<Vec<HistoryItem>>>,
    seen: Mutex<Vec<Vec<HistoryItem>>>,
}
impl FakeModel {
    fn new(turns: Vec<Vec<HistoryItem>>) -> Self {
        Self {
            turns: Mutex::new(turns.into()),
            seen: Mutex::new(vec![]),
        }
    }
}
struct FakeStream(Option<Vec<HistoryItem>>);
impl ModelStream for FakeStream {
    fn next<'a>(&'a mut self, _: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            Ok(self
                .0
                .take()
                .map(|items| ModelUpdate::Finished { items, usage: None }))
        })
    }
}
impl Model for FakeModel {
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            self.seen
                .lock()
                .map_err(|_| error("test", "lock"))?
                .push(request.history.clone());
            let turn = self
                .turns
                .lock()
                .map_err(|_| error("test", "lock"))?
                .pop_front()
                .ok_or_else(|| error("model_error", "no scripted response"))?;
            Ok(Box::new(FakeStream(Some(turn))) as Box<dyn ModelStream>)
        })
    }
}
struct FakeTool {
    seen: Arc<Mutex<Vec<String>>>,
    fail: bool,
}
impl Tool for FakeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "fake".into(),
            description: "test only".into(),
            input_schema: json!({"type":"object"}),
        }
    }
    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        _: &'a ToolContext,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, ToolResult> {
        Box::pin(async move {
            self.seen
                .lock()
                .map_err(|_| error("test", "lock"))?
                .push(call.call_id.clone());
            if self.fail {
                return Err(error("tool_error", "simulated failure"));
            }
            Ok(ToolResult {
                ok: true,
                data: Some(json!({"call_id":call.call_id})),
                error: None,
                truncated: false,
            })
        })
    }
}
struct FailStore {
    inner: MemoryStore,
    fail_intent: bool,
    fail_finish: bool,
}
impl Store for FailStore {
    fn load_session(&self, id: SessionId) -> Operation<'_, Option<Session>> {
        self.inner.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> Operation<'a, ()> {
        Box::pin(async move {
            if changes.iter().any(|change| {
                matches!(change, StoreChange::ToolIntent { .. }) && self.fail_intent
                    || matches!(change, StoreChange::ToolFinished { .. }) && self.fail_finish
            }) {
                return Err(error("storage_error", "injected failure"));
            }
            self.inner.commit(changes).await
        })
    }
}
fn setup() -> (Session, Run) {
    let session = Session {
        id: SessionId::new(),
        workspace: "/fake".into(),
        work_mode: WorkMode::Work,
        profile: "fake".into(),
        protocol: ModelProtocol::Responses,
        model: "fake".into(),
        history: vec![HistoryItem::UserText("hello".into())],
    };
    let run = Run {
        id: RunId::new(),
        session_id: session.id,
        work_mode: WorkMode::Work,
        permissions: PermissionMode::Auto,
    };
    (session, run)
}
fn call(id: &str, name: &str, args: &str) -> HistoryItem {
    HistoryItem::ToolCall(ToolCall {
        call_id: id.into(),
        name: name.into(),
        arguments: args.into(),
    })
}
fn answer() -> Vec<HistoryItem> {
    vec![HistoryItem::AssistantText("done".into())]
}
fn registry(seen: Arc<Mutex<Vec<String>>>, fail: bool) -> Result<ToolRegistry, BoundaryError> {
    let mut tools = ToolRegistry::default();
    tools.register(Box::new(FakeTool { seen, fail }))?;
    Ok(tools)
}

#[tokio::test]
async fn direct_answer_is_saved() -> Result<(), Box<dyn std::error::Error>> {
    let (mut session, run_id) = setup();
    let store = MemoryStore::default();
    let model = FakeModel::new(vec![answer()]);
    assert_eq!(
        run(
            &mut session,
            &run_id,
            &model,
            &ToolRegistry::default(),
            &store,
            &NoCancel
        )
        .await?,
        "done"
    );
    let saved = store
        .load_session(session.id)
        .await?
        .ok_or("missing session")?;
    assert_eq!(saved.history, session.history);
    Ok(())
}

#[tokio::test]
async fn tools_are_serial_and_results_reach_next_model_turn()
-> Result<(), Box<dyn std::error::Error>> {
    let (mut session, run_id) = setup();
    let store = MemoryStore::default();
    let model = FakeModel::new(vec![
        vec![call("first", "fake", "{}"), call("second", "fake", "{}")],
        answer(),
    ]);
    let seen = Arc::new(Mutex::new(vec![]));
    assert_eq!(
        run(
            &mut session,
            &run_id,
            &model,
            &registry(seen.clone(), false)?,
            &store,
            &NoCancel
        )
        .await?,
        "done"
    );
    assert_eq!(*seen.lock().map_err(|_| "lock")?, vec!["first", "second"]);
    let histories = model.seen.lock().map_err(|_| "lock")?;
    let results: Vec<_> = histories[1]
        .iter()
        .filter_map(|h| {
            if let HistoryItem::ToolResult { call_id, result } = h {
                Some((call_id.as_str(), result.ok))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(results, vec![("first", true), ("second", true)]);
    let changes = store.changes()?;
    let order: Vec<_> = changes
        .iter()
        .filter_map(|c| match c {
            StoreChange::ToolIntent { call, .. } => Some(format!("intent:{}", call.call_id)),
            StoreChange::ToolFinished { call_id, .. } => Some(format!("finished:{call_id}")),
            _ => None,
        })
        .collect();
    assert_eq!(
        order,
        [
            "intent:first",
            "finished:first",
            "intent:second",
            "finished:second"
        ]
    );
    Ok(())
}

#[tokio::test]
async fn invalid_unknown_and_failed_calls_receive_paired_results()
-> Result<(), Box<dyn std::error::Error>> {
    let (mut session, run_id) = setup();
    let store = MemoryStore::default();
    let model = FakeModel::new(vec![
        vec![
            call("a", "missing", "{}"),
            call("b", "fake", "{"),
            call("c", "fake", "{}"),
        ],
        answer(),
    ]);
    let seen = Arc::new(Mutex::new(vec![]));
    run(
        &mut session,
        &run_id,
        &model,
        &registry(seen.clone(), true)?,
        &store,
        &NoCancel,
    )
    .await?;
    assert_eq!(*seen.lock().map_err(|_| "lock")?, vec!["c"]);
    let results: Vec<_> = session
        .history
        .iter()
        .filter_map(|h| match h {
            HistoryItem::ToolResult { call_id, result } => Some((
                call_id.as_str(),
                result.error.as_ref().map(|e| e.code.as_str()),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        vec![
            ("a", Some("unknown_tool")),
            ("b", Some("invalid_args")),
            ("c", Some("tool_error"))
        ]
    );
    Ok(())
}

#[tokio::test]
async fn persistence_failures_stop_dispatch_or_next_turn() -> Result<(), Box<dyn std::error::Error>>
{
    for fail_intent in [true, false] {
        let (mut session, run_id) = setup();
        let store = FailStore {
            inner: MemoryStore::default(),
            fail_intent,
            fail_finish: !fail_intent,
        };
        let model = FakeModel::new(vec![
            vec![call("a", "fake", "{}"), call("b", "fake", "{}")],
            answer(),
        ]);
        let seen = Arc::new(Mutex::new(vec![]));
        let err = run(
            &mut session,
            &run_id,
            &model,
            &registry(seen.clone(), false)?,
            &store,
            &NoCancel,
        )
        .await
        .expect_err("storage failure must stop run");
        assert_eq!(err.0.code, "storage_error");
        assert_eq!(
            *seen.lock().map_err(|_| "lock")?,
            if fail_intent { vec![] } else { vec!["a"] }
        );
        assert_eq!(model.seen.lock().map_err(|_| "lock")?.len(), 1);
    }
    Ok(())
}

#[tokio::test]
async fn memory_commit_is_atomic_on_invalid_change() -> Result<(), Box<dyn std::error::Error>> {
    let (session, _) = setup();
    let store = MemoryStore::default();
    let changes = [
        StoreChange::Session(session.clone()),
        StoreChange::History {
            session_id: SessionId::new(),
            item: HistoryItem::UserText("bad".into()),
        },
    ];
    assert!(store.commit(&changes).await.is_err());
    assert!(store.load_session(session.id).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn model_failure_is_not_reported_as_completion() {
    let (mut session, run_id) = setup();
    let store = MemoryStore::default();
    let model = FakeModel::new(vec![]);
    let result = run(
        &mut session,
        &run_id,
        &model,
        &ToolRegistry::default(),
        &store,
        &NoCancel,
    )
    .await;
    assert!(
        matches!(result, Err(BoundaryError(ErrorInfo { code, .. }, _)) if code == "model_error")
    );
}

#[test]
fn duplicate_registration_does_not_replace_first_tool() -> Result<(), Box<dyn std::error::Error>> {
    let first = Arc::new(Mutex::new(vec![]));
    let mut tools = registry(first, false)?;
    let second = Arc::new(Mutex::new(vec![]));
    assert!(
        tools
            .register(Box::new(FakeTool {
                seen: second,
                fail: true
            }))
            .is_err()
    );
    assert_eq!(tools.0.len(), 1);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn active_budget_pauses_and_limits_count_attempts() {
    let mut budget = Budget::new(Limits {
        active_time: Duration::from_secs(10),
        model_requests: 2,
        tool_calls: 1,
        ..Limits::default()
    });
    assert!(budget.request());
    assert!(budget.request());
    assert!(!budget.request());
    assert!(budget.tool());
    assert!(!budget.tool());
    tokio::time::advance(Duration::from_secs(4)).await;
    budget.pause();
    tokio::time::advance(Duration::from_secs(100)).await;
    assert_eq!(budget.remaining(), Duration::from_secs(6));
    budget.resume();
    tokio::time::advance(Duration::from_secs(6)).await;
    assert_eq!(budget.remaining(), Duration::ZERO);
}

#[test]
fn retry_rules_stop_after_partial_and_cap_retry_after() {
    assert_eq!(
        retry_delay(
            "transient_connection",
            0,
            false,
            None,
            Duration::from_millis(50)
        ),
        Some(Duration::from_millis(1050))
    );
    assert_eq!(
        retry_delay(
            "rate_limited",
            1,
            false,
            Some(Duration::from_secs(60)),
            Duration::ZERO
        ),
        Some(Duration::from_secs(30))
    );
    assert_eq!(
        retry_delay("server_error", 2, false, None, Duration::ZERO),
        None
    );
    assert_eq!(
        retry_delay("server_error", 0, true, None, Duration::ZERO),
        None
    );
    assert_eq!(
        retry_delay("authentication", 0, false, None, Duration::ZERO),
        None
    );
}

struct RetryModel {
    starts: Mutex<usize>,
}
impl Model for RetryModel {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            let mut starts = self.starts.lock().map_err(|_| error("test", "lock"))?;
            *starts += 1;
            if *starts < 3 {
                return Err(error("transient_connection", "try again"));
            }
            Ok(Box::new(FakeStream(Some(answer()))) as Box<dyn ModelStream>)
        })
    }
}

#[tokio::test(start_paused = true)]
async fn retries_are_bounded_and_count_toward_request_budget()
-> Result<(), Box<dyn std::error::Error>> {
    let (mut session, run_id) = setup();
    let model = RetryModel {
        starts: Mutex::new(0),
    };
    let store = MemoryStore::default();
    let started = Instant::now();
    assert_eq!(
        run_with_limits(
            &mut session,
            &run_id,
            &model,
            &ToolRegistry::default(),
            &store,
            &NoCancel,
            Limits {
                retry_jitter_max: Duration::ZERO,
                ..Limits::default()
            }
        )
        .await?,
        "done"
    );
    assert_eq!(*model.starts.lock().map_err(|_| "lock")?, 3);
    assert_eq!(started.elapsed(), Duration::from_secs(3));
    let (mut session, run_id) = setup();
    let model = RetryModel {
        starts: Mutex::new(0),
    };
    let limits = Limits {
        model_requests: 2,
        ..Limits::default()
    };
    let failure = run_with_limits(
        &mut session,
        &run_id,
        &model,
        &ToolRegistry::default(),
        &MemoryStore::default(),
        &NoCancel,
        limits,
    )
    .await
    .expect_err("request budget should stop retry");
    assert_eq!(failure.0.code, "limit_reached");
    assert_eq!(*model.starts.lock().map_err(|_| "lock")?, 2);
    Ok(())
}

struct PartialModel {
    starts: Mutex<usize>,
}
struct PartialStream {
    first: bool,
}
impl Model for PartialModel {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            *self.starts.lock().map_err(|_| error("test", "lock"))? += 1;
            Ok(Box::new(PartialStream { first: true }) as Box<dyn ModelStream>)
        })
    }
}
impl ModelStream for PartialStream {
    fn next<'a>(&'a mut self, _: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            if self.first {
                self.first = false;
                Ok(Some(ModelUpdate::TextDelta("partial".into())))
            } else {
                Err(error("server_error", "broken stream"))
            }
        })
    }
}

#[tokio::test(start_paused = true)]
async fn partial_failure_is_saved_without_retry_or_tool_execution()
-> Result<(), Box<dyn std::error::Error>> {
    let (mut session, run_id) = setup();
    let model = PartialModel {
        starts: Mutex::new(0),
    };
    let store = MemoryStore::default();
    let failure = run(
        &mut session,
        &run_id,
        &model,
        &ToolRegistry::default(),
        &store,
        &NoCancel,
    )
    .await
    .expect_err("partial stream must fail");
    assert_eq!(failure.0.code, "server_error");
    assert_eq!(*model.starts.lock().map_err(|_| "lock")?, 1);
    let saved = store
        .load_session(session.id)
        .await?
        .ok_or("missing session")?;
    assert!(
        matches!(saved.history.last(), Some(HistoryItem::AssistantPartial(text)) if text == "partial")
    );
    Ok(())
}

struct NeverModel;
impl Model for NeverModel {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(pending())
    }
}
struct AlreadyCancelled;
impl Cancellation for AlreadyCancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }
}
#[tokio::test(start_paused = true)]
async fn cancellation_and_connection_timeout_stop_the_model()
-> Result<(), Box<dyn std::error::Error>> {
    let (mut session, run_id) = setup();
    let store = MemoryStore::default();
    let cancelled = run(
        &mut session,
        &run_id,
        &NeverModel,
        &ToolRegistry::default(),
        &store,
        &AlreadyCancelled,
    )
    .await
    .expect_err("must cancel");
    assert_eq!(cancelled.0.code, "cancelled");
    assert!(store.load_session(session.id).await?.is_none());
    let (mut session, run_id) = setup();
    let start = Instant::now();
    let timeout = run(
        &mut session,
        &run_id,
        &NeverModel,
        &ToolRegistry::default(),
        &MemoryStore::default(),
        &NoCancel,
    )
    .await
    .expect_err("must time out");
    assert_eq!(timeout.0.code, "model_connect_timeout");
    assert_eq!(start.elapsed(), Duration::from_secs(10));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn outcomes_distinguish_completion_failure_and_budget_limit() {
    let (mut session, id) = setup();
    let completed = run_outcome(
        &mut session,
        &id,
        &FakeModel::new(vec![answer()]),
        &ToolRegistry::default(),
        &MemoryStore::default(),
        &NoCancel,
        Limits::default(),
    )
    .await;
    assert_eq!(completed.status, crate::contract::RunStatus::Completed);
    assert_eq!(completed.text.as_deref(), Some("done"));
    let (mut session, id) = setup();
    let failed = run_outcome(
        &mut session,
        &id,
        &PartialModel {
            starts: Mutex::new(0),
        },
        &ToolRegistry::default(),
        &MemoryStore::default(),
        &NoCancel,
        Limits::default(),
    )
    .await;
    assert_eq!(failed.status, crate::contract::RunStatus::Failed);
    assert_eq!(failed.text.as_deref(), Some("partial"));
    let (mut session, id) = setup();
    let limited = run_outcome(
        &mut session,
        &id,
        &FakeModel::new(vec![]),
        &ToolRegistry::default(),
        &MemoryStore::default(),
        &NoCancel,
        Limits {
            model_requests: 0,
            ..Limits::default()
        },
    )
    .await;
    assert_eq!(limited.status, crate::contract::RunStatus::LimitReached);
}

struct NeverTool;
impl Tool for NeverTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "never".into(),
            description: "test".into(),
            input_schema: json!({"type":"object"}),
        }
    }
    fn execute<'a>(
        &'a self,
        _: &'a ToolCall,
        _: &'a ToolContext,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, ToolResult> {
        Box::pin(pending())
    }
}
struct DelayedCancel;
impl Cancellation for DelayedCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {
            sleep(Duration::from_secs(1)).await;
        })
    }
}
#[tokio::test(start_paused = true)]
async fn cancelling_or_timing_out_a_running_tool_marks_it_unknown()
-> Result<(), Box<dyn std::error::Error>> {
    for cancel_first in [false, true] {
        let (mut session, run_id) = setup();
        let model = FakeModel::new(vec![vec![
            call("a", "never", "{}"),
            call("b", "never", "{}"),
        ]]);
        let store = MemoryStore::default();
        let mut tools = ToolRegistry::default();
        tools.register(Box::new(NeverTool))?;
        let timeout = Limits {
            tool_timeout: Duration::from_secs(2),
            ..Limits::default()
        };
        let cancel: &dyn Cancellation = if cancel_first {
            &DelayedCancel
        } else {
            &NoCancel
        };
        let err = run_with_limits(
            &mut session,
            &run_id,
            &model,
            &tools,
            &store,
            cancel,
            timeout,
        )
        .await
        .expect_err("must stop");
        assert_eq!(
            err.0.code,
            if cancel_first {
                "cancelled"
            } else {
                "tool_timeout"
            }
        );
        let changes = store.changes()?;
        assert!(changes.iter().any(|c| matches!(c, StoreChange::ToolState { call_id, state: crate::contract::ToolState::Unknown, .. } if call_id == "a")));
        assert!(
            !changes
                .iter()
                .any(|c| matches!(c, StoreChange::ToolIntent { call, .. } if call.call_id == "b"))
        );
    }
    Ok(())
}

#[tokio::test]
async fn same_tool_and_normalized_arguments_stop_after_three_failures()
-> Result<(), Box<dyn std::error::Error>> {
    let (mut session, run_id) = setup();
    let model = FakeModel::new(vec![vec![
        call("a", "fake", "{\"x\":1,\"y\":2}"),
        call("b", "fake", "{\"y\":2,\"x\":1}"),
        call("c", "fake", "{\"x\":1,\"y\":2}"),
        call("d", "fake", "{}"),
    ]]);
    let seen = Arc::new(Mutex::new(vec![]));
    let err = run(
        &mut session,
        &run_id,
        &model,
        &registry(seen.clone(), true)?,
        &MemoryStore::default(),
        &NoCancel,
    )
    .await
    .expect_err("repeated failures must stop");
    assert_eq!(err.0.code, "repeated_tool_failure");
    assert_eq!(*seen.lock().map_err(|_| "lock")?, vec!["a", "b", "c"]);
    Ok(())
}

struct IdleModel;
impl Model for IdleModel {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async { Ok(Box::new(IdleStream) as Box<dyn ModelStream>) })
    }
}
struct IdleStream;
impl ModelStream for IdleStream {
    fn next<'a>(&'a mut self, _: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(pending())
    }
}
#[tokio::test(start_paused = true)]
async fn idle_total_and_active_deadlines_are_enforced() {
    for (limits, expected, elapsed) in [
        (
            Limits {
                model_idle: Duration::from_secs(3),
                ..Limits::default()
            },
            "model_timeout",
            3,
        ),
        (
            Limits {
                model_total: Duration::from_secs(4),
                ..Limits::default()
            },
            "model_timeout",
            4,
        ),
        (
            Limits {
                active_time: Duration::from_secs(5),
                ..Limits::default()
            },
            "limit_reached",
            5,
        ),
    ] {
        let (mut session, run_id) = setup();
        let start = Instant::now();
        let err = run_with_limits(
            &mut session,
            &run_id,
            &IdleModel,
            &ToolRegistry::default(),
            &MemoryStore::default(),
            &NoCancel,
            limits,
        )
        .await
        .expect_err("must stop");
        assert_eq!(err.0.code, expected);
        assert_eq!(start.elapsed(), Duration::from_secs(elapsed));
    }
}

#[tokio::test(start_paused = true)]
async fn cancellation_before_or_during_model_is_reported_as_cancelled() {
    for cancel in [&AlreadyCancelled as &dyn Cancellation, &DelayedCancel] {
        let (mut session, id) = setup();
        let result = run_outcome(
            &mut session,
            &id,
            &NeverModel,
            &ToolRegistry::default(),
            &MemoryStore::default(),
            cancel,
            Limits::default(),
        )
        .await;
        assert_eq!(result.status, crate::contract::RunStatus::Cancelled);
    }
}

struct RetryAfterModel {
    calls: Mutex<u8>,
}
impl Model for RetryAfterModel {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            let mut count = self.calls.lock().map_err(|_| error("test", "lock"))?;
            *count += 1;
            if *count == 1 {
                return Err(BoundaryError(
                    ErrorInfo {
                        code: "rate_limited".into(),
                        message: "busy".into(),
                    },
                    Some(Duration::from_secs(60)),
                ));
            }
            Ok(Box::new(FakeStream(Some(answer()))) as Box<dyn ModelStream>)
        })
    }
}
#[tokio::test(start_paused = true)]
async fn retry_after_metadata_is_capped_in_the_loop() -> Result<(), Box<dyn std::error::Error>> {
    let (mut session, id) = setup();
    let model = RetryAfterModel {
        calls: Mutex::new(0),
    };
    let start = Instant::now();
    let limits = Limits {
        retry_jitter_max: Duration::ZERO,
        ..Limits::default()
    };
    run_with_limits(
        &mut session,
        &id,
        &model,
        &ToolRegistry::default(),
        &MemoryStore::default(),
        &NoCancel,
        limits,
    )
    .await?;
    assert_eq!(start.elapsed(), Duration::from_secs(30));
    Ok(())
}

struct CooperativeTool(Arc<std::sync::atomic::AtomicBool>);
impl Tool for CooperativeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "cooperative".into(),
            description: "test".into(),
            input_schema: json!({"type":"object"}),
        }
    }
    fn execute<'a>(
        &'a self,
        _: &'a ToolCall,
        _: &'a ToolContext,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, ToolResult> {
        Box::pin(async move {
            cancel.cancelled().await;
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            Err(error("cancelled", "cleaned up"))
        })
    }
}
#[tokio::test(start_paused = true)]
async fn tool_receives_cancellation_and_can_finish_cleanup()
-> Result<(), Box<dyn std::error::Error>> {
    let (mut session, id) = setup();
    let was_cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut tools = ToolRegistry::default();
    tools.register(Box::new(CooperativeTool(was_cancelled.clone())))?;
    let model = FakeModel::new(vec![vec![call("a", "cooperative", "{}")]]);
    let start = Instant::now();
    let limits = Limits {
        tool_timeout: Duration::from_secs(2),
        ..Limits::default()
    };
    let failure = run_with_limits(
        &mut session,
        &id,
        &model,
        &tools,
        &MemoryStore::default(),
        &NoCancel,
        limits,
    )
    .await
    .expect_err("tool must time out");
    assert_eq!(failure.0.code, "tool_timeout");
    assert!(was_cancelled.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(start.elapsed(), Duration::from_secs(2));
    Ok(())
}

#[tokio::test]
async fn failure_never_returns_partial_text_from_an_earlier_run() {
    let (mut session, id) = setup();
    session
        .history
        .push(HistoryItem::AssistantPartial("old output".into()));
    let result = run_outcome(
        &mut session,
        &id,
        &FakeModel::new(vec![]),
        &ToolRegistry::default(),
        &MemoryStore::default(),
        &NoCancel,
        Limits::default(),
    )
    .await;
    assert_eq!(result.status, crate::contract::RunStatus::Failed);
    assert_eq!(result.text, None);
}
