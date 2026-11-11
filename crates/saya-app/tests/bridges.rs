use saya_app::{
    events::{ChannelSink, EventWriter},
    interaction::{Request, RunCancellation, input_channel},
};
use saya_core::{
    boundary::Cancellation,
    contract::{EventPayload, ModelProtocol, RunId, Session, SessionId, WorkMode},
    input::{InputHandler, InputRequest},
};
use std::{sync::Arc, time::Duration};

#[tokio::test]
async fn input_disconnect_and_dropped_replies_fail_closed() {
    let request = InputRequest {
        question: "choose".into(),
        options: vec![],
    };
    let cancel = RunCancellation::new();
    let (bridge, mut receiver) = input_channel(1).unwrap();
    let asking = bridge.ask(&request, &cancel);
    tokio::pin!(asking);
    tokio::select! { result = &mut asking => panic!("unexpected reply {result:?}"), message = receiver.recv() => { drop(message); } }
    assert!(asking.await.unwrap().is_none());
    drop(receiver);
    assert!(bridge.ask(&request, &cancel).await.unwrap().is_none());
}

#[tokio::test]
async fn full_input_queue_remains_cancellable_and_late_reply_has_no_receiver() {
    let request = InputRequest {
        question: "choose".into(),
        options: vec![],
    };
    let cancel = RunCancellation::new();
    let (bridge, mut receiver) = input_channel(1).unwrap();
    let first = bridge.ask(&request, &cancel);
    let second = bridge.ask(&request, &cancel);
    tokio::pin!(first, second);
    tokio::select! { result = &mut first => panic!("unexpected reply {result:?}"), () = tokio::task::yield_now() => {} }
    tokio::select! { result = &mut second => panic!("unexpected reply {result:?}"), () = tokio::task::yield_now() => {} }
    cancel.cancel();
    assert!(first.await.unwrap().is_none());
    assert!(second.await.unwrap().is_none());
    let Some(Request::Input(_, reply)) = receiver.recv().await else {
        panic!("missing input")
    };
    assert!(reply.send(Ok(Some("late".into()))).is_err());
    assert!(cancel.is_cancelled());
    cancel.cancelled().await;
}

#[tokio::test]
async fn bounded_event_delivery_preserves_sequence_and_reports_disconnect() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(saya_core::sqlite::SqliteStore::open(root.path(), vec![]).unwrap());
    let session = Session {
        id: SessionId::new(),
        workspace: root.path().into(),
        work_mode: WorkMode::Work,
        profile: "fixture".into(),
        protocol: ModelProtocol::ChatCompletions,
        model: "fake".into(),
        history: vec![],
    };
    store.create_session(&session, "fingerprint").await.unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let writer = EventWriter::new(
        store,
        session.id,
        RunId::new(),
        vec![],
        Arc::new(ChannelSink(sender)),
    );
    let payload = EventPayload::SessionCreated {
        work_mode: session.work_mode,
        permissions: saya_core::contract::PermissionMode::Auto,
        workspace: session.workspace.clone(),
        profile: session.profile.clone(),
    };
    writer.emit(payload.clone(), None, true).await.unwrap();
    let second = writer.emit(payload.clone(), None, true);
    tokio::pin!(second);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut second)
            .await
            .is_err()
    );
    let first = receiver.recv().await.unwrap();
    second.await.unwrap();
    let next = receiver.recv().await.unwrap();
    assert!(next.seq > first.seq);
    writer.emit(payload.clone(), None, true).await.unwrap();
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        writer.emit(payload.clone(), None, true),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.0.code, "event_consumer_stalled");
    drop(receiver);
    assert_eq!(
        writer.emit(payload, None, true).await.unwrap_err().0.code,
        "event_consumer_closed"
    );
}

#[tokio::test]
async fn failure_terminal_is_unique_and_a_new_writer_starts_after_its_reserved_range() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(saya_core::sqlite::SqliteStore::open(root.path(), vec![]).unwrap());
    let session = Session {
        id: SessionId::new(),
        workspace: root.path().into(),
        work_mode: WorkMode::Work,
        profile: "fixture".into(),
        protocol: ModelProtocol::ChatCompletions,
        model: "fixture".into(),
        history: vec![],
    };
    store.create_session(&session, "fingerprint").await.unwrap();
    let run = RunId::new();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
    let writer = EventWriter::new(
        store.clone(),
        session.id,
        run,
        vec![],
        Arc::new(ChannelSink(sender)),
    );
    let result = saya_core::contract::RunResult {
        schema_version: 1,
        session_id: Some(session.id),
        run_id: Some(run),
        status: saya_core::contract::RunStatus::Failed,
        stop_reason: Some("storage_error".into()),
        text: None,
        error: None,
        usage: None,
        artifacts: vec![],
        persistence: None,
    };
    writer.emit_failure(result.clone()).await.unwrap();
    writer.emit_failure(result).await.unwrap();
    let terminal = receiver.recv().await.unwrap();
    assert_eq!(terminal.session_id, session.id);
    assert_eq!(terminal.run_id, Some(run));
    assert!(receiver.try_recv().is_err());
    let payload = EventPayload::SessionResumed {
        work_mode: WorkMode::Work,
        permissions: saya_core::contract::PermissionMode::Auto,
        workspace: session.workspace,
        profile: session.profile,
    };
    assert_eq!(
        writer
            .emit(payload.clone(), None, false)
            .await
            .unwrap_err()
            .0
            .code,
        "output_error"
    );
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let next = EventWriter::new(
        store,
        session.id,
        RunId::new(),
        vec![],
        Arc::new(ChannelSink(sender)),
    );
    next.emit(payload, None, false).await.unwrap();
    assert!(receiver.recv().await.unwrap().seq > terminal.seq);
}

#[tokio::test]
async fn dynamic_event_registry_buffers_split_text_and_refreshes_before_delivery() {
    use saya_app::events::EventModel;
    use saya_core::{
        boundary::{Model, ModelRequest, ModelStream, ModelUpdate, Operation},
        contract::{HistoryItem, ModelPurpose},
        credentials::SecretRegistry,
    };
    use std::collections::VecDeque;

    struct Fixed;
    struct Stream(VecDeque<ModelUpdate>);
    impl Model for Fixed {
        fn start<'a>(
            &'a self,
            _: &'a ModelRequest,
            _: &'a dyn Cancellation,
        ) -> Operation<'a, Box<dyn ModelStream>> {
            Box::pin(async {
                Ok(Box::new(Stream(VecDeque::from([
                    ModelUpdate::TextDelta("rotation-".into()),
                    ModelUpdate::TextDelta("synthetic-value".into()),
                    ModelUpdate::Finished {
                        items: vec![HistoryItem::AssistantText("answer".into())],
                        usage: None,
                    },
                ]))) as Box<dyn ModelStream>)
            })
        }
    }
    impl ModelStream for Stream {
        fn next<'a>(&'a mut self, _: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
            Box::pin(async move { Ok(self.0.pop_front()) })
        }
    }
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(saya_core::sqlite::SqliteStore::open(root.path(), vec![]).unwrap());
    let session = Session {
        id: SessionId::new(),
        workspace: root.path().into(),
        work_mode: WorkMode::Work,
        profile: "fixture".into(),
        protocol: ModelProtocol::ChatCompletions,
        model: "fake".into(),
        history: vec![],
    };
    store.create_session(&session, "fingerprint").await.unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(16);
    let registry = Arc::new(SecretRegistry::default());
    let run = saya_core::contract::Run {
        id: RunId::new(),
        session_id: session.id,
        work_mode: WorkMode::Work,
        permissions: saya_core::contract::PermissionMode::Auto,
    };
    saya_core::boundary::Store::commit(
        &*store,
        &[saya_core::boundary::StoreChange::RunStarted(run.clone())],
    )
    .await
    .unwrap();
    let writer = Arc::new(
        EventWriter::new(
            store,
            session.id,
            run.id,
            vec!["static-synthetic-secret".into()],
            Arc::new(ChannelSink(sender)),
        )
        .with_secret_registry(registry.clone())
        .unwrap(),
    );
    assert!(Arc::ptr_eq(&writer.secret_registry().unwrap(), &registry));
    assert!(
        registry
            .snapshot()
            .unwrap()
            .contains(&"static-synthetic-secret".into())
    );
    let model = EventModel(Arc::new(Fixed), writer.clone());
    let request = ModelRequest {
        request_id: "rotation".into(),
        purpose: ModelPurpose::Execution,
        profile: "fixture".into(),
        instructions: String::new(),
        dynamic_instructions: None,
        max_output_tokens: None,
        history: vec![],
        media: Default::default(),
        tools: vec![],
    };
    let cancel = RunCancellation::new();
    let mut stream = model.start(&request, &cancel).await.unwrap();
    assert!(matches!(
        receiver.recv().await.unwrap().payload,
        EventPayload::ModelStarted { .. }
    ));
    stream.next(&cancel).await.unwrap();
    assert!(receiver.try_recv().is_err());
    registry
        .register(&["rotation-synthetic-value".into()])
        .unwrap();
    stream.next(&cancel).await.unwrap();
    assert!(receiver.try_recv().is_err());
    stream.next(&cancel).await.unwrap();
    let event = receiver.recv().await.unwrap();
    assert!(
        matches!(event.payload, EventPayload::ModelTextDelta { ref text, .. } if text == "[REDACTED]")
    );
    writer
        .emit(
            EventPayload::ModelTextDelta {
                request_id: "fresh-snapshot".into(),
                text: "rotation-synthetic-value static-synthetic-secret".into(),
            },
            None,
            false,
        )
        .await
        .unwrap();
    let event = receiver.recv().await.unwrap();
    let encoded = serde_json::to_string(&event).unwrap();
    assert!(!encoded.contains("rotation-synthetic-value"));
    assert!(!encoded.contains("static-synthetic-secret"));
}
