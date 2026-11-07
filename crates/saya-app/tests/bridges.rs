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
