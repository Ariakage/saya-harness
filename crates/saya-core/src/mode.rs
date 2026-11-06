//! Coordination of user-requested work mode changes. This state machine does
//! not itself persist changes, dispatch tools, or enforce permissions.
use crate::contract::{BoundaryError, ErrorInfo, WorkMode};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct ModeController(Arc<Mutex<State>>);
struct State {
    current: WorkMode,
    pending: Option<WorkMode>,
    in_call: bool,
    revision: u64,
}
#[derive(Clone, Copy)]
pub struct ModeSnapshot {
    pub current: WorkMode,
    pub pending: Option<WorkMode>,
    pub revision: u64,
}

fn poisoned() -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "mode_state_error".into(),
            message: "mode lock poisoned".into(),
        },
        None,
        None,
    )
}
impl ModeController {
    /// Create a controller with the selected initial mode and no pending request.
    pub fn new(initial: WorkMode) -> Self {
        Self(Arc::new(Mutex::new(State {
            current: initial,
            pending: None,
            in_call: false,
            revision: 0,
        })))
    }
    /// Queue a user request during a call, or apply it immediately while idle.
    /// Returns an error if the state lock is poisoned. Model suggestions must
    /// not invoke this method without an explicit user request.
    pub fn request(&self, target: WorkMode) -> Result<(), BoundaryError> {
        let mut s = self.0.lock().map_err(|_| poisoned())?;
        if s.in_call {
            s.pending = Some(target);
        } else if s.current != target {
            s.current = target;
            s.revision += 1;
        }
        Ok(())
    }
    /// Read the current and pending modes and the confirmation revision.
    /// Returns an error if the state lock is poisoned.
    pub fn snapshot(&self) -> Result<ModeSnapshot, BoundaryError> {
        let s = self.0.lock().map_err(|_| poisoned())?;
        Ok(ModeSnapshot {
            current: s.current,
            pending: s.pending,
            revision: s.revision,
        })
    }
    /// Atomically begin a call if the snapshot is still current. Returns false
    /// if another call or request raced, or an error if the lock is poisoned.
    /// The caller must persist the mode and check authorization before dispatch.
    pub fn begin(&self, revision: u64) -> Result<bool, BoundaryError> {
        let mut s = self.0.lock().map_err(|_| poisoned())?;
        if s.in_call || s.revision != revision || s.pending.is_some() {
            return Ok(false);
        }
        s.in_call = true;
        Ok(true)
    }
    /// Mark an invocation complete, including on failure, timeout or cancel,
    /// and apply the last pending request. The caller must persist the change
    /// before dispatching again; lock poisoning returns an error.
    pub fn finish(&self) -> Result<(), BoundaryError> {
        let mut s = self.0.lock().map_err(|_| poisoned())?;
        s.in_call = false;
        if let Some(mode) = s.pending.take()
            && s.current != mode
        {
            s.current = mode;
            s.revision += 1;
        }
        Ok(())
    }
    /// Check whether a confirmation revision still matches, with no call or
    /// pending change. Returns an error if the state lock is poisoned; this
    /// check alone does not grant authorization.
    pub fn approval_is_current(&self, revision: u64) -> Result<bool, BoundaryError> {
        let s = self.0.lock().map_err(|_| poisoned())?;
        Ok(!s.in_call && s.pending.is_none() && s.revision == revision)
    }
}
