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
    contexts: HashMap<SessionId, crate::context_store::ContextCheckpoint>,
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

    fn memory_exposures(
        &self,
        id: SessionId,
    ) -> Operation<'_, Vec<crate::memory_context::MemoryExposure>> {
        Box::pin(async move {
            let state = self
                .state
                .lock()
                .map_err(|_| failure("memory store lock poisoned"))?;
            Ok(state
                .changes
                .iter()
                .filter_map(|change| match change {
                    StoreChange::MemoryExposure(value) if value.session_id == id => {
                        Some(value.clone())
                    }
                    _ => None,
                })
                .collect())
        })
    }
    fn context_checkpoint(
        &self,
        id: SessionId,
    ) -> Operation<'_, Option<crate::context_store::ContextCheckpoint>> {
        Box::pin(async move {
            Ok(self
                .state
                .lock()
                .map_err(|_| failure("memory store lock poisoned"))?
                .contexts
                .get(&id)
                .cloned())
        })
    }
    fn output_references(
        &self,
        id: SessionId,
    ) -> Operation<'_, Vec<crate::output_store::OutputMetadata>> {
        Box::pin(async move {
            let state = self
                .state
                .lock()
                .map_err(|_| failure("memory store lock poisoned"))?;
            Ok(state
                .changes
                .iter()
                .filter_map(|change| match change {
                    StoreChange::OutputReferences { outputs, .. } => Some(outputs),
                    _ => None,
                })
                .flatten()
                .filter(|output| output.scope.session_id == id)
                .cloned()
                .collect())
        })
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> Operation<'a, ()> {
        Box::pin(async move {
            let mut guard = self
                .state
                .lock()
                .map_err(|_| failure("memory store lock poisoned"))?;
            crate::output_store::validate_changes(changes)?;
            let mut next = guard.clone();
            for change in changes {
                match change {
                    StoreChange::Session(session) => {
                        next.sessions.insert(session.id, session.clone());
                    }
                    StoreChange::MemoryExposure(value) => {
                        let run = next
                            .runs
                            .get(&value.run_id)
                            .ok_or_else(|| failure("run not found"))?;
                        if run.session_id != value.session_id {
                            return Err(failure("memory exposure run mismatch"));
                        }
                        value.validate_source(
                            next.sessions
                                .get(&value.session_id)
                                .ok_or_else(|| failure("session not found"))?,
                        )?;
                    }
                    StoreChange::ContextCheckpoint { run_id, checkpoint } => {
                        let run = next
                            .runs
                            .get(run_id)
                            .ok_or_else(|| failure("run not found"))?;
                        let session = next
                            .sessions
                            .get(&run.session_id)
                            .ok_or_else(|| failure("session not found"))?;
                        checkpoint.validate_source(session)?;
                        if checkpoint.follows(next.contexts.get(&session.id))? {
                            next.contexts
                                .insert(session.id, checkpoint.as_ref().clone());
                        }
                    }
                    StoreChange::RunStarted(run) => {
                        if !next.sessions.contains_key(&run.session_id) {
                            return Err(failure("session not found"));
                        }
                        if next.runs.insert(run.id, run.clone()).is_some() {
                            return Err(failure("duplicate run"));
                        }
                    }
                    StoreChange::ProjectContext { run_id, .. }
                    | StoreChange::Activity { run_id, .. }
                    | StoreChange::RunMetrics { run_id, .. } => {
                        if !next.runs.contains_key(run_id) {
                            return Err(failure("run not found"));
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
                    StoreChange::OutputReferences {
                        run_id,
                        call_id,
                        outputs,
                    } => {
                        let run = next
                            .runs
                            .get(run_id)
                            .ok_or_else(|| failure("run not found"))?;
                        let result = changes
                            .iter()
                            .find_map(|change| match change {
                                StoreChange::ToolFinished {
                                    run_id: producer,
                                    call_id: id,
                                    result,
                                } if producer == run_id && id == call_id => Some(result),
                                _ => None,
                            })
                            .ok_or_else(|| failure("producer result missing"))?;
                        crate::output_store::validate_references(
                            outputs,
                            run.session_id,
                            *run_id,
                            call_id,
                            result,
                        )?;
                        if next.changes.iter().any(|change| {
                            matches!(change,
                            StoreChange::OutputReferences {run_id: producer,call_id:id,..}
                            if producer == run_id && id == call_id)
                        }) {
                            return Err(failure("producer outputs already registered"));
                        }
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
                    StoreChange::ApprovalRequested(_)
                    | StoreChange::Event(_)
                    | StoreChange::ReservedEvent(_) => {}
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
