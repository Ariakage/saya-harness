//! Host-owned, human-readable global memory with explicit evidence and scope.
//!
//! Opening and reading a missing store have no filesystem side effects. Source
//! references are opaque identities. They never grant access to host paths.
use crate::{
    contract::{BoundaryError, ErrorInfo},
    credentials::SecretRegistry,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use unicode_normalization::UnicodeNormalization;

mod disk;
mod document;
mod projection;
mod proposal;
mod query;
mod state;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_query;
#[cfg(test)]
mod tests_transaction;
mod transaction;

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
fn check_cancelled(cancel: &AtomicBool) -> Result<(), BoundaryError> {
    if cancel.load(Ordering::Acquire) {
        return Err(error("cancelled", "Memory read was cancelled"));
    }
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn encode_bounded<T: Serialize>(value: &T, max: usize) -> Result<Vec<u8>, BoundaryError> {
    struct Bounded {
        bytes: Vec<u8>,
        max: usize,
        exceeded: bool,
    }
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.bytes.len().saturating_add(bytes.len()) > self.max {
                self.exceeded = true;
                return Err(std::io::Error::other("memory byte limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded {
        bytes: Vec::new(),
        max,
        exceeded: false,
    };
    if serde_json::to_writer(&mut output, value).is_err() {
        return Err(if output.exceeded {
            error(
                "memory_limit",
                "Memory encoded metadata exceeds its byte limit",
            )
        } else {
            error("memory_format", "Memory metadata cannot be encoded")
        });
    }
    Ok(output.bytes)
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_id(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
        && value.len() > prefix.len()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Normalization catches declared textual duplicates. It does not infer whether
/// two paraphrases have the same meaning.
pub fn fact_fingerprint(text: &str) -> String {
    let normalized = text.nfc().flat_map(char::to_lowercase).collect::<String>();
    digest(
        normalized
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .as_bytes(),
    )
}

pub fn new_entry_id() -> String {
    format!("mem_{}", uuid::Uuid::now_v7().simple())
}
pub fn new_fact_id() -> String {
    format!("fact_{}", uuid::Uuid::now_v7().simple())
}
pub fn new_source_id() -> String {
    format!("src_{}", uuid::Uuid::now_v7().simple())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryLimits {
    pub max_index_bytes: usize,
    pub max_resource_bytes: usize,
    pub max_total_resource_bytes: usize,
    pub max_entries: usize,
    pub max_facts: usize,
    pub max_sources: usize,
    pub max_resources: usize,
    pub max_fact_bytes: usize,
    pub max_history_versions: usize,
}
impl Default for MemoryLimits {
    fn default() -> Self {
        Self {
            max_index_bytes: 512 * 1024,
            max_resource_bytes: 64 * 1024,
            max_total_resource_bytes: 1024 * 1024,
            max_entries: 256,
            max_facts: 1024,
            max_sources: 1024,
            max_resources: 64,
            max_fact_bytes: 16 * 1024,
            max_history_versions: 32,
        }
    }
}
impl MemoryLimits {
    pub fn validate(&self) -> Result<(), BoundaryError> {
        if self.max_index_bytes == 0
            || self.max_index_bytes > 4 * 1024 * 1024
            || self.max_resource_bytes == 0
            || self.max_resource_bytes > 1024 * 1024
            || self.max_total_resource_bytes < self.max_resource_bytes
            || self.max_total_resource_bytes > 16 * 1024 * 1024
            || self.max_entries == 0
            || self.max_entries > 4096
            || self.max_facts == 0
            || self.max_facts > 16384
            || self.max_sources == 0
            || self.max_sources > 16384
            || self.max_resources == 0
            || self.max_resources > 1024
            || self.max_fact_bytes == 0
            || self.max_fact_bytes > self.max_index_bytes
            || self.max_history_versions == 0
            || self.max_history_versions > 256
        {
            return Err(error("memory_config", "Memory limits are invalid"));
        }
        Ok(())
    }
}

/// The host constructs this identity from the actual workspace. Model input must
/// never select a different workspace identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceIdentity {
    pub id: String,
}
impl WorkspaceIdentity {
    pub fn from_path(path: &Path) -> Result<Self, BoundaryError> {
        disk::workspace_identity(path)
    }
    pub fn validate(&self) -> Result<(), BoundaryError> {
        if self
            .id
            .strip_prefix("ws_")
            .is_none_or(|value| !valid_digest(value))
        {
            return Err(error("memory_scope", "Workspace identity is invalid"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryScope {
    Global,
    Workspace { identity: String },
}
impl MemoryScope {
    pub fn visible_in(&self, workspace: &WorkspaceIdentity) -> bool {
        matches!(self, Self::Global)
            || matches!(self, Self::Workspace { identity } if identity == &workspace.id)
    }
    fn supports(&self, scope: &Self) -> bool {
        self == &Self::Global || self == scope
    }
    fn validate(&self) -> Result<(), BoundaryError> {
        match self {
            Self::Global => Ok(()),
            Self::Workspace { identity } => WorkspaceIdentity {
                id: identity.clone(),
            }
            .validate(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    ExplicitPreference,
    UserCorrection,
    ObservedFact,
    TaskDecision,
    Inference,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySourceKind {
    User,
    Session,
    Tool,
    Import,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySource {
    pub id: String,
    pub kind: MemorySourceKind,
    pub scope: MemoryScope,
    pub reference: String,
    pub revision: String,
    pub observed_at: String,
    pub description: String,
    #[serde(default)]
    pub metadata_withheld: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySourceRef {
    pub source_id: String,
    pub revision: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryFact {
    pub id: String,
    pub kind: MemoryKind,
    pub text: String,
    pub sources: Vec<MemorySourceRef>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub derived_from: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryEntry {
    pub id: String,
    pub title: String,
    pub scope: MemoryScope,
    pub updated_at: String,
    pub facts: Vec<MemoryFact>,
    #[serde(default)]
    pub resources: Vec<String>,
}
/// Tombstones contain identities and fingerprints, never forgotten plaintext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryEntryMetadataSuppression {
    pub entry_id: String,
    pub title_fingerprint: String,
    #[serde(default)]
    pub updated_at_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryTombstone {
    pub operation_id: String,
    pub scope: MemoryScope,
    pub fact_ids: Vec<String>,
    pub fingerprints: Vec<String>,
    #[serde(default)]
    pub source_ids: Vec<String>,
    #[serde(default)]
    pub resources: Vec<String>,
    #[serde(default)]
    pub entry_metadata: Vec<MemoryEntryMetadataSuppression>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRetraction {
    pub source_id: String,
    pub revision: String,
    pub scope: MemoryScope,
    #[serde(default)]
    pub resources: Vec<String>,
    #[serde(default)]
    pub entry_metadata: Vec<MemoryEntryMetadataSuppression>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryDocument {
    pub schema_version: u32,
    #[serde(default)]
    pub sources: Vec<MemorySource>,
    #[serde(default)]
    pub entries: Vec<MemoryEntry>,
    #[serde(default)]
    pub tombstones: Vec<MemoryTombstone>,
    #[serde(default)]
    pub retractions: Vec<SourceRetraction>,
}
impl Default for MemoryDocument {
    fn default() -> Self {
        Self {
            schema_version: 1,
            sources: Vec::new(),
            entries: Vec::new(),
            tombstones: Vec::new(),
            retractions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryResourceInfo {
    pub path: String,
    pub revision: String,
    pub bytes: usize,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryResource {
    pub info: MemoryResourceInfo,
    pub text: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySnapshot {
    pub exists: bool,
    pub revision: String,
    pub workspace: WorkspaceIdentity,
    pub entries: Vec<MemoryEntry>,
    pub sources: Vec<MemorySource>,
    pub resources: Vec<MemoryResourceInfo>,
    pub forget_revision: String,
}

pub use projection::{
    MEMORY_CONTEXT_INSTRUCTIONS, MemoryProjection, MemoryProjectionOptions, MemoryProjectionStatus,
    MemoryRoute,
};
pub use proposal::{MemoryChange, MemoryProposal};
pub use query::{
    MemoryCursor, MemoryFragment, MemoryMatchField, MemoryQueryLimits, MemoryReadOptions,
    MemoryReadPage, MemoryReference, MemorySearchHit, MemorySearchOptions, MemorySearchPage,
};
pub use state::{MemoryHistoryVersion, MemoryOperationReceipt};
pub use transaction::{MemoryCommitOutcome, MemoryCommitStatus};

#[derive(Clone)]
pub struct GlobalMemoryStore {
    root: PathBuf,
    workspace: WorkspaceIdentity,
    limits: MemoryLimits,
    secrets: Arc<SecretRegistry>,
}
impl GlobalMemoryStore {
    /// Performs configuration validation only. Missing roots stay missing.
    pub fn open(
        root: &Path,
        workspace: WorkspaceIdentity,
        limits: MemoryLimits,
        secrets: Arc<SecretRegistry>,
    ) -> Result<Self, BoundaryError> {
        limits.validate()?;
        workspace.validate()?;
        if !root.is_absolute() {
            return Err(error(
                "memory_config",
                "Memory root must be an absolute host path",
            ));
        }
        let root_text = root
            .to_str()
            .ok_or_else(|| error("memory_config", "Memory root must be UTF-8"))?;
        if crate::redaction::contains_secret(root_text, &secrets.snapshot()?) {
            return Err(error(
                "sensitive_data",
                "Memory root contains protected data",
            ));
        }
        Ok(Self {
            root: root.to_owned(),
            workspace,
            limits,
            secrets,
        })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn limits(&self) -> MemoryLimits {
        self.limits
    }
    /// Host approval covers every indexed topic and the store's internal
    /// transaction targets. Query and mutation callers bind this revision.
    pub fn authorization_paths(&self) -> Result<(String, Vec<PathBuf>), BoundaryError> {
        let loaded = self.load()?;
        let mut paths = std::collections::BTreeSet::from([
            self.root.join("MEMORY.md"),
            self.root.join(".saya-memory.lock"),
            self.root.join(state::STATE_PATH),
            self.root.join(state::JOURNAL_PATH),
            self.root.join(state::STAGE_PATH),
            self.root.join("memory/.saya-history"),
        ]);
        for entry in &loaded.document.entries {
            for path in &entry.resources {
                paths.insert(self.root.join(path));
            }
        }
        for version in self.history()? {
            paths.insert(
                self.root
                    .join(format!("memory/.saya-history/{}.md", version.version)),
            );
        }
        if let Some(version) = &loaded.index_digest {
            paths.insert(self.root.join(format!("memory/.saya-history/{version}.md")));
        }
        let paths = paths
            .into_iter()
            .map(|path| crate::execution::normalize_path(&path))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((loaded.snapshot.revision, paths))
    }
    pub fn workspace(&self) -> &WorkspaceIdentity {
        &self.workspace
    }
    fn binding(&self) -> String {
        let encoded = format!(
            "{}\0{}\0{:?}",
            self.root.display(),
            self.workspace.id,
            self.limits
        );
        digest(encoded.as_bytes())
    }
    pub fn snapshot(&self) -> Result<MemorySnapshot, BoundaryError> {
        Ok(self.load()?.snapshot)
    }
    pub fn snapshot_cancellable(
        &self,
        cancel: &AtomicBool,
    ) -> Result<MemorySnapshot, BoundaryError> {
        Ok(self.load_cancellable(cancel)?.snapshot)
    }
    /// A topic must belong to a currently visible entry. The optional revision
    /// binds a follow-up read to the caller's earlier snapshot.
    pub fn read_resource(
        &self,
        path: &str,
        expected_revision: Option<&str>,
    ) -> Result<MemoryResource, BoundaryError> {
        self.read_resource_cancellable(path, expected_revision, &AtomicBool::new(false))
    }
    pub fn read_resource_cancellable(
        &self,
        path: &str,
        expected_revision: Option<&str>,
        cancel: &AtomicBool,
    ) -> Result<MemoryResource, BoundaryError> {
        check_cancelled(cancel)?;
        disk::validate_resource_path(path)?;
        let loaded = self.load_cancellable(cancel)?;
        check_revision(expected_revision, &loaded.snapshot.revision)?;
        loaded
            .resources
            .get(path)
            .cloned()
            .ok_or_else(|| error("memory_not_found", "Memory resource is not visible"))
    }
    pub fn parse_document(&self, text: &str) -> Result<MemoryDocument, BoundaryError> {
        document::parse(text, self.limits, &self.secrets.snapshot()?)
    }
    pub fn render_document(&self, document: &MemoryDocument) -> Result<String, BoundaryError> {
        document::render(document, self.limits, &self.secrets.snapshot()?)
    }
    fn render_index(&self, document: &MemoryDocument) -> Result<String, BoundaryError> {
        document::validate(document, self.limits, &self.secrets.snapshot()?)?;
        let mut index = document.clone();
        index.tombstones.clear();
        index.retractions.clear();
        self.render_document(&index)
    }
    pub fn propose(
        &self,
        expected_revision: &str,
        operation_id: &str,
        change: MemoryChange,
    ) -> Result<MemoryProposal, BoundaryError> {
        proposal::propose(self, expected_revision, operation_id, change)
    }
    /// Bind the host's normalized request identity for durable idempotency.
    /// The host derives this digest from request, session, workspace and stable
    /// evidence identities, and authorizes the resulting complete proposal.
    pub fn propose_host_request(
        &self,
        expected_revision: &str,
        operation_id: &str,
        host_request_digest: &str,
        change: MemoryChange,
    ) -> Result<MemoryProposal, BoundaryError> {
        self.propose(expected_revision, operation_id, change)?
            .bind_host_request(host_request_digest)
    }
    /// Return bounded, redacted review data using this store's current known
    /// credentials. Ownership keeps the original value out of an error result.
    pub fn redact_review_value(
        &self,
        mut value: serde_json::Value,
    ) -> Result<serde_json::Value, BoundaryError> {
        let _ = encode_bounded(&value, state::max_state_bytes(self.limits))?;
        let secrets = self.secrets.snapshot()?;
        crate::redaction::redact(&mut value, &secrets).map_err(|_| {
            error(
                "sensitive_data",
                "Memory review data cannot be safely redacted",
            )
        })?;
        Ok(value)
    }
}

fn check_revision(expected: Option<&str>, actual: &str) -> Result<(), BoundaryError> {
    if expected.is_some_and(|revision| revision != actual) {
        return Err(error(
            "memory_conflict",
            "Memory changed since the requested revision",
        ));
    }
    Ok(())
}

struct Loaded {
    document: MemoryDocument,
    snapshot: MemorySnapshot,
    resources: BTreeMap<String, MemoryResource>,
    index_digest: Option<String>,
}
