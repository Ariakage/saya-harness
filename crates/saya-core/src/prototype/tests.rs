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
    assert!(matches!(result, Err(BoundaryError(ErrorInfo { code, .. })) if code == "model_error"));
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
