//! Volatile storage for controlled tests; this is not durable persistence.

use crate::boundary::{Operation, Store, StoreChange};
use crate::contract::{BoundaryError, ErrorInfo, Run, RunId, Session, SessionId, ToolState};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Clone, Default)]
struct State {
    sessions: HashMap<SessionId, Session>,
    runs: HashMap<RunId, Run>,
    tools: HashMap<(RunId, String), ToolState>,
    changes: Vec<StoreChange>,
}

/// A transactional in-memory store. State is lost when this value is dropped.
#[derive(Default)]
pub struct MemoryStore {
    state: Mutex<State>,
}

fn failure(message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "storage_error".into(),
            message: message.into(),
        },
        None,
        None,
    )
}

impl Store for MemoryStore {
    fn load_session(&self, id: SessionId) -> Operation<'_, Option<Session>> {
        Box::pin(async move {
            let state = self
                .state
                .lock()
                .map_err(|_| failure("memory store lock poisoned"))?;
            Ok(state.sessions.get(&id).cloned())
        })
    }

    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> Operation<'a, ()> {
        Box::pin(async move {
            let mut guard = self
                .state
                .lock()
                .map_err(|_| failure("memory store lock poisoned"))?;
            let mut next = guard.clone();
            for change in changes {
                match change {
                    StoreChange::Session(session) => {
                        next.sessions.insert(session.id, session.clone());
                    }
                    StoreChange::RunStarted(run) => {
                        if !next.sessions.contains_key(&run.session_id) {
                            return Err(failure("session not found"));
                        }
                        if next.runs.insert(run.id, run.clone()).is_some() {
                            return Err(failure("duplicate run"));
                        }
                    }
                    StoreChange::History { session_id, item } => {
                        next.sessions
                            .get_mut(session_id)
                            .ok_or_else(|| failure("session not found"))?
                            .history
                            .push(item.clone());
                    }
                    StoreChange::ToolIntent { run_id, call } => {
                        if !next.runs.contains_key(run_id) {
                            return Err(failure("run not found"));
                        }
                        if next
                            .tools
                            .insert((*run_id, call.call_id.clone()), ToolState::Running)
                            .is_some()
                        {
                            return Err(failure("duplicate tool call"));
                        }
                    }
                    StoreChange::ToolFinished {
                        run_id,
                        call_id,
                        result,
                    } => {
                        let state = next
                            .tools
                            .get_mut(&(*run_id, call_id.clone()))
                            .ok_or_else(|| failure("tool intent not found"))?;
                        if *state != ToolState::Running {
                            return Err(failure("tool call is not running"));
                        }
                        *state = if result.ok {
                            ToolState::Succeeded
                        } else {
                            ToolState::Failed
                        };
                    }
                    StoreChange::ToolState {
                        run_id,
                        call_id,
                        state,
                    } => {
                        *next
                            .tools
                            .get_mut(&(*run_id, call_id.clone()))
                            .ok_or_else(|| failure("tool intent not found"))? = *state;
                    }
                    StoreChange::RunFinished(result) => {
                        let id = result.run_id.ok_or_else(|| failure("missing run ID"))?;
                        if !next.runs.contains_key(&id) {
                            return Err(failure("run not found"));
                        }
                    }
                    StoreChange::Event(_) => {}
                }
                next.changes.push(change.clone());
            }
            *guard = next;
            Ok(())
        })
    }
}

#[cfg(test)]
impl MemoryStore {
    pub(crate) fn changes(&self) -> Result<Vec<StoreChange>, BoundaryError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| failure("memory store lock poisoned"))?
            .changes
            .clone())
    }
}
