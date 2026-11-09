//! Durable checkpoint payload and source validation shared by session stores.
use crate::{
    compaction::{CompactionRecord, validate_committed},
    context_builder::{self, ContextView},
    contract::{BoundaryError, ErrorInfo, Session},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpoint {
    pub view: ContextView,
    pub record: CompactionRecord,
}

fn conflict() -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "compaction_conflict".into(),
            message: "Durable compaction source or checkpoint pointer changed".into(),
        },
        None,
        None,
    )
}

impl ContextCheckpoint {
    pub fn validate(&self) -> Result<(), BoundaryError> {
        validate_committed(&self.view, &self.record)
    }

    /// Verify a stored v1 record independently of the current prompt release.
    /// An older prompt remains historical data and is reprojected from raw history.
    pub fn validate_persisted(&self) -> Result<(), BoundaryError> {
        use sha2::{Digest, Sha256};
        if self.record.prompt_version.is_empty()
            || self.record.prompt_version.len() > 128
            || self.record.prompt_version.chars().any(char::is_control)
            || self.record.prompt_sha256.len() != 64
            || !self
                .record
                .prompt_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(conflict());
        }
        let mut record = self.record.clone();
        record.prompt_version = crate::compaction::PROMPT_VERSION.into();
        record.prompt_sha256 = Sha256::digest(crate::compaction::COMPACT_INSTRUCTIONS.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        validate_committed(&self.view, &record)
    }

    /// Check the immutable source in the same transaction as pointer publication.
    pub fn validate_source(&self, session: &Session) -> Result<(), BoundaryError> {
        self.validate()?;
        let source = &self.record.source;
        if self.record.session_id != session.id
            || self.record.profile != session.profile
            || source.identity.protocol != session.protocol
            || source.identity.model != session.model
            || source.revision != session.history.len() as u64
            || source.item_count != session.history.len()
            || source.history_sha256 != context_builder::history_digest(&session.history)?
            || self.view.covered_prefix > session.history.len()
            || self.view.source_prefix_sha256
                != context_builder::history_digest(&session.history[..self.view.covered_prefix])?
            || !context_builder::group_history(session.protocol, &session.history)?
                .is_safe_prefix(self.view.covered_prefix, &[])
        {
            return Err(conflict());
        }
        for group in &self.record.retained_groups {
            if group.range.end > session.history.len()
                || context_builder::history_digest(
                    &session.history[group.range.start..group.range.end],
                )? != group.source_sha256
            {
                return Err(conflict());
            }
        }
        Ok(())
    }

    /// Retry the same publication idempotently. Concurrent successors are fenced.
    pub fn follows(&self, current: Option<&Self>) -> Result<bool, BoundaryError> {
        if let Some(current) = current
            && self.view.checkpoint_id == current.view.checkpoint_id
        {
            return if self == current {
                Ok(false)
            } else {
                Err(conflict())
            };
        }
        if self.record.previous_checkpoint.as_deref()
            != current.map(|value| value.view.checkpoint_id.as_str())
        {
            return Err(conflict());
        }
        Ok(true)
    }
}
