//! Host-only provenance checks for retrieving memory-dependent retired output.
//! Apply current permission/Manual Approval normally, then run this guard again
//! against fresh retirements before reading or exposing a body. Retained audit
//! history is left intact. This component neither deletes artifacts nor grants
//! an operation authority.
use crate::{
    background::{BackgroundJobOrigin, JobStatus, child_prefix},
    context_builder::{
        GroupRetirement, RetirementOrigin, group_history, history_digest, item_digest,
    },
    contract::{
        BoundaryError, ErrorInfo, HistoryItem, RunId, Session, SessionId, ToolCall, ToolResult,
    },
    output_store::OutputMetadata,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn error(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(crate::text::is_terminal_control)
}

#[derive(Debug)]
pub(crate) struct RetiredOutputAcl {
    session_id: SessionId,
    history_sha256: String,
    active: bool,
    retired_calls: BTreeSet<String>,
    known_calls: BTreeSet<String>,
    retired_children: BTreeSet<String>,
    known_children: BTreeSet<String>,
}
impl RetiredOutputAcl {
    /// `retirements` must come from authenticated host memory-exposure records.
    /// Structural source checks are repeated here to reject stale projections.
    pub(crate) fn build(
        session: &Session,
        retirements: &[GroupRetirement],
    ) -> Result<Self, BoundaryError> {
        if session.history.len() > 65_536 || retirements.len() > 65_536 {
            return Err(error(
                "output_provenance",
                "Retired output provenance exceeds its item limit",
            ));
        }
        let history_sha256 = history_digest(&session.history)?;
        let mut acl = Self {
            session_id: session.id,
            history_sha256,
            active: !retirements.is_empty(),
            retired_calls: BTreeSet::new(),
            known_calls: BTreeSet::new(),
            retired_children: BTreeSet::new(),
            known_children: BTreeSet::new(),
        };
        if retirements.is_empty() {
            return Ok(acl);
        }
        let grouping = group_history(session.protocol, &session.history)?;
        let groups = grouping
            .groups
            .iter()
            .map(|group| (group.range.start, group))
            .collect::<BTreeMap<_, _>>();
        let mut seen = BTreeSet::new();
        let mut memory_revision = None;
        for retirement in retirements {
            let group = groups.get(&retirement.range.start).ok_or_else(|| {
                error(
                    "output_provenance",
                    "Retired output group is absent from current history",
                )
            })?;
            if !group.closed
                || group.range != retirement.range
                || !seen.insert(retirement.range.start)
                || retirement.origins.is_empty()
                || retirement.origins.len() > 256
                || !valid_hash(&retirement.current_memory_revision)
                || memory_revision
                    .is_some_and(|revision: &str| revision != retirement.current_memory_revision)
                || retirement.range.end > session.history.len()
                || history_digest(&session.history[retirement.range.start..retirement.range.end])?
                    != retirement.source_sha256
                || session.history[retirement.range.start..retirement.range.end]
                    .iter()
                    .any(|item| matches!(item, HistoryItem::UserText(_)))
            {
                return Err(error(
                    "output_provenance",
                    "Retired output group is not a current source-bound assistant group",
                ));
            }
            memory_revision = Some(retirement.current_memory_revision.as_str());
            for origin in &retirement.origins {
                validate_origin(session, retirement, origin)?;
            }
            for call_id in &group.call_ids {
                if !identity(call_id) {
                    return Err(error(
                        "output_provenance",
                        "Retired producer identity is invalid",
                    ));
                }
                acl.retired_calls.insert(call_id.clone());
            }
        }
        for group in &grouping.groups {
            for call_id in &group.call_ids {
                if !identity(call_id) {
                    return Err(error("output_provenance", "Producer identity is invalid"));
                }
                acl.known_calls.insert(call_id.clone());
            }
        }
        acl.retired_children = acl
            .retired_calls
            .iter()
            .map(|call| child_prefix(call))
            .collect();
        acl.known_children = acl
            .known_calls
            .iter()
            .map(|call| child_prefix(call))
            .collect();
        Ok(acl)
    }
    pub(crate) fn is_active(&self) -> bool {
        self.active
    }
    fn validate_session(&self, session: &Session) -> Result<(), BoundaryError> {
        if session.id != self.session_id || history_digest(&session.history)? != self.history_sha256
        {
            return Err(error(
                "stale_approval",
                "Output provenance differs from the current session snapshot",
            ));
        }
        Ok(())
    }
    fn retired(&self, call_id: &str) -> bool {
        self.retired_calls.contains(call_id)
            || self
                .retired_children
                .iter()
                .any(|prefix| call_id.starts_with(prefix))
    }
    fn known(&self, call_id: &str) -> bool {
        self.known_calls.contains(call_id)
            || self
                .known_children
                .iter()
                .any(|prefix| call_id.starts_with(prefix))
    }

    /// Return None only when the existing tool may retrieve its normal body.
    /// Use durable host registration metadata, never result-supplied output_refs.
    pub(crate) fn output_result(
        &self,
        session: &Session,
        call: &ToolCall,
        registered: &[OutputMetadata],
    ) -> Result<Option<ToolResult>, BoundaryError> {
        if !matches!(call.name.as_str(), "output_read" | "output_search") || !self.is_active() {
            return Ok(None);
        }
        self.validate_session(session)?;
        let reference = selector(call, "output_ref", 160)?;
        if !reference
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || registered.len() > 65_536
        {
            return Err(error(
                "output_provenance",
                "Output registration identity is invalid or exceeds its limit",
            ));
        }
        let mut selected = None;
        for metadata in registered
            .iter()
            .filter(|metadata| metadata.output_ref == reference)
        {
            if metadata.scope.session_id != self.session_id
                || selected.is_some_and(|previous: &OutputMetadata| {
                    previous.scope != metadata.scope || previous.sha256 != metadata.sha256
                })
            {
                return Err(error(
                    "output_provenance",
                    "Output registration is ambiguous or belongs to another session",
                ));
            }
            selected = Some(metadata);
        }
        let Some(metadata) = selected else {
            return Ok(Some(unavailable()));
        };
        if !identity(&metadata.scope.call_id) {
            return Err(error(
                "output_provenance",
                "Output producer identity is invalid",
            ));
        }
        if !self.retired(&metadata.scope.call_id) {
            return Ok(if self.known(&metadata.scope.call_id) {
                None
            } else {
                Some(unavailable())
            });
        }
        // Preview strings, chunks, search hits, call arguments and error text
        // are deliberately absent from this fixed mechanical projection.
        Ok(Some(ToolResult {
            ok: false,
            data: Some(
                json!({"output_ref":reference,"stream":metadata.scope.stream,"body_withheld":true,"reason":"memory_context_changed",
            "total_bytes_seen":metadata.total_bytes_seen,"retained_bytes":metadata.retained_bytes,"artifact_complete":metadata.artifact_complete,"completion":metadata.completion,"coverage":metadata.coverage}),
            ),
            error: Some(ErrorInfo {
                code: "memory_source_retired".into(),
                message: "Historical output content depends on retired memory context".into(),
            }),
            truncated: true,
        }))
    }

    /// Job identities come from BackgroundJobs::origin, never a model-facing
    /// job_id field found in an old or external ToolResult.
    pub(crate) fn background_result(
        &self,
        session: &Session,
        current_run: RunId,
        call: &ToolCall,
        origin: Option<&BackgroundJobOrigin>,
        status: Option<&JobStatus>,
    ) -> Result<Option<ToolResult>, BoundaryError> {
        if call.name != "background_status" || !self.is_active() {
            return Ok(None);
        }
        self.validate_session(session)?;
        let _ = selector(call, "job_id", 256)?;
        let Some(status) = status else {
            if origin.is_some() {
                return Err(error(
                    "output_provenance",
                    "Background source exists without its status",
                ));
            }
            return Ok(None);
        };
        let Some(origin) = origin else {
            return Ok(Some(background_projection(status, "unverified_job_origin")));
        };
        if origin.session_id != self.session_id
            || origin.run_id != current_run
            || !identity(&origin.call_id)
        {
            return Err(error(
                "output_provenance",
                "Background output belongs to another session or Run",
            ));
        }
        if self.retired(&origin.call_id) {
            return Ok(Some(background_projection(
                status,
                "memory_context_changed",
            )));
        }
        Ok(if self.known(&origin.call_id) {
            None
        } else {
            Some(background_projection(status, "unverified_job_origin"))
        })
    }
}

fn validate_origin(
    session: &Session,
    retirement: &GroupRetirement,
    origin: &RetirementOrigin,
) -> Result<(), BoundaryError> {
    match origin {
        RetirementOrigin::InjectedMemoryTurn {
            request_id_sha256,
            memory_revision,
        } => {
            if !valid_hash(request_id_sha256) || !valid_hash(memory_revision) {
                return Err(error(
                    "output_provenance",
                    "Injected memory source identity is invalid",
                ));
            }
        }
        RetirementOrigin::MemoryToolCall {
            call_index,
            call_sha256,
            result_index,
            result_sha256,
            memory_revision,
        } => {
            if *call_index < retirement.range.start
                || *result_index >= retirement.range.end
                || call_index >= result_index
                || !valid_hash(memory_revision)
            {
                return Err(error(
                    "output_provenance",
                    "Native memory source range is invalid",
                ));
            }
            let (Some(call_item), Some(result_item)) = (
                session.history.get(*call_index),
                session.history.get(*result_index),
            ) else {
                return Err(error(
                    "output_provenance",
                    "Native memory source is missing",
                ));
            };
            let (HistoryItem::ToolCall(call), HistoryItem::ToolResult { call_id, .. }) =
                (call_item, result_item)
            else {
                return Err(error(
                    "output_provenance",
                    "Native memory source has no logical call/result pair",
                ));
            };
            if call.call_id != *call_id
                || !matches!(
                    call.name.as_str(),
                    "memory_read"
                        | "memory_search"
                        | "memory_update"
                        | "memory_forget"
                        | "memory_evidence"
                )
                || item_digest(call_item)? != *call_sha256
                || item_digest(result_item)? != *result_sha256
            {
                return Err(error(
                    "output_provenance",
                    "Native memory source differs from its bound history",
                ));
            }
        }
    }
    Ok(())
}
fn selector(call: &ToolCall, key: &str, max: usize) -> Result<String, BoundaryError> {
    if call.arguments.len() > 64 * 1024 {
        return Err(error(
            "invalid_args",
            "Output retrieval arguments exceed their limit",
        ));
    }
    let args: Value = serde_json::from_str(&call.arguments)
        .map_err(|_| error("invalid_args", "Output retrieval arguments are invalid"))?;
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= max
                && !value.chars().any(crate::text::is_terminal_control)
        })
        .map(str::to_owned)
        .ok_or_else(|| error("invalid_args", "Output retrieval selector is invalid"))
}
fn unavailable() -> ToolResult {
    ToolResult {
        ok: false,
        data: None,
        error: Some(ErrorInfo {
            code: "output_provenance_unavailable".into(),
            message: "Historical output provenance must be verified before retrieving content"
                .into(),
        }),
        truncated: false,
    }
}
fn mechanical(result: &ToolResult) -> Value {
    let data = result.data.as_ref();
    json!({"ok":result.ok,"truncated":result.truncated,"error_present":result.error.is_some(),
        "exit_code":data.and_then(|data|data.get("exit_code")).and_then(Value::as_i64).and_then(|value|i32::try_from(value).ok()),
        "timed_out":data.and_then(|data|data.get("timed_out")).and_then(Value::as_bool),"cancelled":data.and_then(|data|data.get("cancelled")).and_then(Value::as_bool)})
}
fn background_projection(status: &JobStatus, reason: &str) -> ToolResult {
    let (state, outcome) = match status {
        JobStatus::Running => ("running", None),
        JobStatus::Finished(result) => ("finished", Some(mechanical(result))),
        JobStatus::Failed(failure) => (
            "failed",
            failure.2.as_ref().map(|result| mechanical(result.as_ref())),
        ),
    };
    ToolResult {
        ok: true,
        data: Some(
            json!({"status":state,"body_withheld":true,"reason":reason,"mechanical_outcome":outcome,"error_present":matches!(status,JobStatus::Failed(_))}),
        ),
        error: None,
        truncated: true,
    }
}

#[cfg(test)]
#[path = "retired_outputs/tests.rs"]
mod tests;
