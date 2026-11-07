//! Shared input and cancellation bridge. Interfaces own rendering and reply senders.
use saya_core::{
    boundary::{Cancellation, Operation},
    contract::{ApprovalRequest, BoundaryError},
    input::{InputHandler, InputRequest},
    runtime::ApprovalHandler,
};
use std::{future::Future, pin::Pin, sync::Arc};
use tokio::sync::{mpsc, oneshot};
pub enum Request {
    Approval(
        ApprovalRequest,
        oneshot::Sender<Result<Option<bool>, BoundaryError>>,
    ),
    Input(
        InputRequest,
        oneshot::Sender<Result<Option<String>, BoundaryError>>,
    ),
}
pub struct Interaction {
    requests: mpsc::Sender<Request>,
}
impl ApprovalHandler for Interaction {
    fn confirm<'a>(
        &'a self,
        request: &'a ApprovalRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Option<bool>> {
        Box::pin(async move {
            let (send, receive) = oneshot::channel();
            tokio::select! { biased;
                () = cancel.cancelled() => return Ok(None),
                result = self.requests.send(Request::Approval(request.clone(), send)) => { if result.is_err() { return Ok(None); } }
            }
            tokio::select! { biased; ()=cancel.cancelled()=>Ok(None), result=receive=>result.unwrap_or(Ok(None)) }
        })
    }
}
impl InputHandler for Interaction {
    fn ask<'a>(
        &'a self,
        request: &'a InputRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Option<String>> {
        Box::pin(async move {
            let (send, receive) = oneshot::channel();
            tokio::select! { biased;
                () = cancel.cancelled() => return Ok(None),
                result = self.requests.send(Request::Input(request.clone(), send)) => { if result.is_err() { return Ok(None); } }
            }
            tokio::select! { biased; ()=cancel.cancelled()=>Ok(None), result=receive=>result.unwrap_or(Ok(None)) }
        })
    }
}
pub struct RunCancellation(tokio::sync::watch::Sender<bool>);
impl Cancellation for RunCancellation {
    fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let mut rx = self.0.subscribe();
            while !*rx.borrow_and_update() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
        })
    }
}
impl Default for RunCancellation {
    fn default() -> Self {
        Self::new()
    }
}
impl RunCancellation {
    pub fn new() -> Self {
        Self(tokio::sync::watch::channel(false).0)
    }
    pub fn cancel(&self) -> bool {
        self.0.send_replace(true)
    }
}
/// Create a bounded bridge. Dropped receivers and replies fail closed.
/// Current approval validity is always enforced by the core dispatch gate.
pub fn input_channel(
    capacity: usize,
) -> Result<(Arc<Interaction>, mpsc::Receiver<Request>), BoundaryError> {
    if capacity == 0 || capacity > 1024 {
        return Err(BoundaryError(
            saya_core::contract::ErrorInfo {
                code: "invalid_input_capacity".into(),
                message: "input capacity must be 1..=1024".into(),
            },
            None,
            None,
        ));
    }
    let (sender, receiver) = mpsc::channel(capacity);
    Ok((Arc::new(Interaction { requests: sender }), receiver))
}
