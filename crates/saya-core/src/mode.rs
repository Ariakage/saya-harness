//! Coordination of user-requested work mode changes. This state machine does
//! not itself persist changes, dispatch tools, or enforce permissions.
use crate::contract::{BoundaryError, ErrorInfo, EventPayload, WorkMode};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

const MAX_MODE_EVENTS: usize = 256;
#[derive(Clone)]
pub(crate) struct ModeEvent {
    pub sequence: u64,
    pub payload: EventPayload,
}

#[derive(Clone)]
pub struct ModeController(Arc<Mutex<State>>);
struct State {
    current: WorkMode,
    pending: Option<WorkMode>,
    active: usize,
    exclusive: bool,
    revision: u64,
    events: VecDeque<ModeEvent>,
    event_sequence: u64,
    record_events: bool,
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
    pub(crate) fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    /// Create a controller with the selected initial mode and no pending request.
    pub fn new(initial: WorkMode) -> Self {
        Self::create(initial, false)
    }
    pub(crate) fn recording(initial: WorkMode) -> Self {
        Self::create(initial, true)
    }
    fn create(initial: WorkMode, record_events: bool) -> Self {
        Self(Arc::new(Mutex::new(State {
            current: initial,
            pending: None,
            active: 0,
            exclusive: false,
            revision: 0,
            events: VecDeque::new(),
            event_sequence: 0,
            record_events,
        })))
    }
    /// Queue a user request during a call, or apply it immediately while idle.
    /// Requesting the current mode withdraws a pending change without blocking
    /// new shared calls; withdrawing a change invalidates older approvals.
    /// Returns an error if the state lock is poisoned or the bounded event
    /// journal is full. Rejected requests preserve the current state. Model
    /// suggestions must not invoke this method without an explicit user request.
    pub fn request(&self, target: WorkMode) -> Result<(), BoundaryError> {
        self.request_after(target, || {})
    }
    pub(crate) fn request_after(
        &self,
        target: WorkMode,
        before: impl FnOnce(),
    ) -> Result<(), BoundaryError> {
        let mut s = self.0.lock().map_err(|_| poisoned())?;
        let immediate = s.active == 0 && s.current != target;
        let pending = s.active != 0 && s.current != target;
        // A queued request reserves its eventual Changed entry. Finishing an
        // existing invocation can therefore always apply the pending mode.
        let needed = 1 + usize::from(immediate) + usize::from(pending);
        if s.record_events
            && (s.events.len() + needed > MAX_MODE_EVENTS
                || s.event_sequence.checked_add(needed as u64).is_none())
        {
            return Err(BoundaryError(
                ErrorInfo {
                    code: "mode_event_limit".into(),
                    message: "mode event journal is full".into(),
                },
                None,
                None,
            ));
        }
        before();
        let from = s.current;
        s.record(EventPayload::ModeChangeRequested { from, to: target });
        if s.current == target {
            if s.pending.take().is_some() {
                s.revision += 1;
            }
        } else if s.active != 0 {
            s.pending = Some(target);
        } else {
            s.current = target;
            s.revision += 1;
            s.record(EventPayload::ModeChanged { from, to: target });
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
    pub(crate) fn event_snapshot(&self) -> Result<(ModeSnapshot, Vec<ModeEvent>), BoundaryError> {
        let s = self.0.lock().map_err(|_| poisoned())?;
        Ok((
            ModeSnapshot {
                current: s.current,
                pending: s.pending,
                revision: s.revision,
            },
            s.events.iter().cloned().collect(),
        ))
    }
    pub(crate) fn acknowledge_events(&self, through: u64) -> Result<(), BoundaryError> {
        let mut s = self.0.lock().map_err(|_| poisoned())?;
        while s
            .events
            .front()
            .is_some_and(|event| event.sequence <= through)
        {
            s.events.pop_front();
        }
        Ok(())
    }
    /// Atomically begin a call if the snapshot is still current. Returns false
    /// if another call or request raced, or an error if the lock is poisoned.
    /// The caller must persist the mode and check authorization before dispatch.
    pub fn begin(&self, revision: u64) -> Result<bool, BoundaryError> {
        let mut s = self.0.lock().map_err(|_| poisoned())?;
        if s.active != 0 || s.revision != revision || s.pending.is_some() {
            return Ok(false);
        }
        s.active = 1;
        s.exclusive = true;
        Ok(true)
    }
    /// Begin a concurrently dispatched call at the same mode revision.
    /// Pending mode changes and exclusive calls prevent new dispatch.
    pub fn begin_shared(&self, revision: u64) -> Result<bool, BoundaryError> {
        let mut s = self.0.lock().map_err(|_| poisoned())?;
        if s.exclusive || s.revision != revision || s.pending.is_some() {
            return Ok(false);
        }
        let Some(active) = s.active.checked_add(1) else {
            return Ok(false);
        };
        s.active = active;
        Ok(true)
    }
    /// Hold a host cleanup barrier before requesting a mode transition. The
    /// owner must release it with `finish`, including on cancellation or error.
    pub(crate) fn hold_transition(&self) -> Result<(), BoundaryError> {
        let mut state = self.0.lock().map_err(|_| poisoned())?;
        state.active = state.active.checked_add(1).ok_or_else(|| {
            BoundaryError(
                ErrorInfo {
                    code: "mode_busy".into(),
                    message: "mode transition guard capacity exhausted".into(),
                },
                None,
                None,
            )
        })?;
        Ok(())
    }
    pub(crate) fn active_invocations(&self) -> Result<usize, BoundaryError> {
        Ok(self.0.lock().map_err(|_| poisoned())?.active)
    }
    /// Mark an invocation complete, including on failure, timeout or cancel,
    /// and apply the last pending request. The caller must persist the change
    /// before dispatching again; lock poisoning returns an error.
    pub fn finish(&self) -> Result<(), BoundaryError> {
        let mut s = self.0.lock().map_err(|_| poisoned())?;
        if s.active == 0 {
            return Err(poisoned());
        }
        s.active -= 1;
        if s.active != 0 {
            return Ok(());
        }
        s.exclusive = false;
        if let Some(mode) = s.pending.take()
            && s.current != mode
        {
            let from = s.current;
            s.current = mode;
            s.revision += 1;
            s.record(EventPayload::ModeChanged { from, to: mode });
        }
        Ok(())
    }
    /// Execute an internal permission-setting change only while no call is active.
    pub(crate) fn while_idle(&self, action: impl FnOnce()) -> Result<bool, BoundaryError> {
        let state = self.0.lock().map_err(|_| poisoned())?;
        if state.active != 0 {
            return Ok(false);
        }
        action();
        Ok(true)
    }
    /// Check whether a confirmation revision still matches, with no call or
    /// pending change. Returns an error if the state lock is poisoned; this
    /// check alone does not grant authorization.
    pub fn approval_is_current(&self, revision: u64) -> Result<bool, BoundaryError> {
        let s = self.0.lock().map_err(|_| poisoned())?;
        Ok(s.active == 0 && s.pending.is_none() && s.revision == revision)
    }
    pub(crate) fn shared_approval_is_current(&self, revision: u64) -> Result<bool, BoundaryError> {
        let s = self.0.lock().map_err(|_| poisoned())?;
        Ok(!s.exclusive && s.pending.is_none() && s.revision == revision)
    }
}
impl State {
    fn record(&mut self, payload: EventPayload) {
        if !self.record_events {
            return;
        }
        self.event_sequence += 1;
        self.events.push_back(ModeEvent {
            sequence: self.event_sequence,
            payload,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_mode_request_does_not_block_concurrent_calls() {
        let modes = ModeController::new(WorkMode::Work);
        assert!(modes.begin_shared(0).unwrap());
        modes.request(WorkMode::Work).unwrap();
        let snapshot = modes.snapshot().unwrap();
        assert!(snapshot.pending.is_none());
        assert_eq!(snapshot.revision, 0);
        assert!(modes.begin_shared(snapshot.revision).unwrap());
        modes.finish().unwrap();
        modes.finish().unwrap();
        assert_eq!(modes.snapshot().unwrap().current, WorkMode::Work);
    }

    #[test]
    fn returning_to_current_mode_clears_pending_and_invalidates_old_approvals() {
        let modes = ModeController::new(WorkMode::Work);
        assert!(modes.begin_shared(0).unwrap());
        modes.request(WorkMode::Code).unwrap();
        assert_eq!(modes.snapshot().unwrap().pending, Some(WorkMode::Code));
        modes.request(WorkMode::Work).unwrap();
        let snapshot = modes.snapshot().unwrap();
        assert!(snapshot.pending.is_none());
        assert_eq!(snapshot.current, WorkMode::Work);
        assert_eq!(snapshot.revision, 1);
        assert!(!modes.shared_approval_is_current(0).unwrap());
        assert!(modes.begin_shared(snapshot.revision).unwrap());
        modes.finish().unwrap();
        modes.finish().unwrap();
        assert_eq!(modes.snapshot().unwrap().current, WorkMode::Work);
        assert!(modes.approval_is_current(snapshot.revision).unwrap());
    }

    #[test]
    fn journal_records_requests_and_only_actual_changes_in_order() {
        let modes = ModeController::recording(WorkMode::Work);
        modes.request(WorkMode::Code).unwrap();
        assert!(modes.begin_shared(1).unwrap());
        modes.request(WorkMode::Plan).unwrap();
        modes.request(WorkMode::Build).unwrap();
        modes.request(WorkMode::Code).unwrap();
        modes.finish().unwrap();
        let (_, events) = modes.event_snapshot().unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.payload.clone())
                .collect::<Vec<_>>(),
            vec![
                EventPayload::ModeChangeRequested {
                    from: WorkMode::Work,
                    to: WorkMode::Code
                },
                EventPayload::ModeChanged {
                    from: WorkMode::Work,
                    to: WorkMode::Code
                },
                EventPayload::ModeChangeRequested {
                    from: WorkMode::Code,
                    to: WorkMode::Plan
                },
                EventPayload::ModeChangeRequested {
                    from: WorkMode::Code,
                    to: WorkMode::Build
                },
                EventPayload::ModeChangeRequested {
                    from: WorkMode::Code,
                    to: WorkMode::Code
                },
            ]
        );
        assert!(
            events
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
    }

    #[test]
    fn full_journal_rejects_requests_and_keeps_the_pending_finish_slot() {
        let modes = ModeController::recording(WorkMode::Work);
        assert!(modes.begin_shared(0).unwrap());
        for index in 0..MAX_MODE_EVENTS - 1 {
            modes
                .request(if index % 2 == 0 {
                    WorkMode::Plan
                } else {
                    WorkMode::Code
                })
                .unwrap();
        }
        let before = modes.snapshot().unwrap();
        let mut called = false;
        assert_eq!(
            modes
                .request_after(WorkMode::Build, || called = true)
                .unwrap_err()
                .0
                .code,
            "mode_event_limit"
        );
        assert!(!called);
        let after = modes.snapshot().unwrap();
        assert_eq!(
            (after.current, after.pending, after.revision),
            (before.current, before.pending, before.revision)
        );
        modes.finish().unwrap();
        let (snapshot, events) = modes.event_snapshot().unwrap();
        assert_eq!(events.len(), MAX_MODE_EVENTS);
        assert_eq!(snapshot.current, before.pending.unwrap());
        assert!(matches!(
            events.last().unwrap().payload,
            EventPayload::ModeChanged { .. }
        ));
        let acknowledged = events[127].sequence;
        modes.acknowledge_events(acknowledged).unwrap();
        modes.request(WorkMode::Build).unwrap();
        let (_, remaining) = modes.event_snapshot().unwrap();
        assert_eq!(remaining.first().unwrap().sequence, acknowledged + 1);
        assert!(remaining.last().unwrap().sequence > events.last().unwrap().sequence);
    }

    #[test]
    fn public_state_machine_remains_usable_without_a_journal_consumer() {
        let modes = ModeController::new(WorkMode::Work);
        for _ in 0..512 {
            modes.request(WorkMode::Code).unwrap();
            modes.request(WorkMode::Work).unwrap();
        }
        assert_eq!(modes.snapshot().unwrap().current, WorkMode::Work);
        assert!(modes.event_snapshot().unwrap().1.is_empty());
    }
}
