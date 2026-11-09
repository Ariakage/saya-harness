//! Selected UTF-8 file checkpoints for a host-bound workspace.
//!
//! Synchronous filesystem work belongs on a blocking worker. Every restore
//! requires a host authorization callback. Publication is atomic per replaced
//! file, with optimistic conflict checks. External writers do not participate
//! in our lock, and a multi-file restore can leave a documented partial result.
use crate::{
    contract::{BoundaryError, ErrorInfo, RunId, SessionId},
    credentials::SecretRegistry,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use uuid::Uuid;

mod disk;
#[cfg(test)]
mod tests;
mod workspace;

const MAX_METADATA_BYTES: usize = 1024 * 1024;
fn error(code: &'static str, message: &'static str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn cancelled(cancel: &AtomicBool) -> Result<(), BoundaryError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(error("cancelled", "Checkpoint operation was cancelled"));
    }
    Ok(())
}
fn valid_id(id: &str) -> bool {
    Uuid::parse_str(id).is_ok_and(|value| value.to_string() == id)
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointLimits {
    pub max_files: usize,
    pub max_file_bytes: usize,
    pub max_snapshot_bytes: usize,
    pub max_checkpoints: usize,
    pub max_store_bytes: u64,
    pub max_diff_bytes: usize,
}
impl Default for CheckpointLimits {
    fn default() -> Self {
        Self {
            max_files: 128,
            max_file_bytes: 1024 * 1024,
            max_snapshot_bytes: 32 * 1024 * 1024,
            max_checkpoints: 128,
            max_store_bytes: 256 * 1024 * 1024,
            max_diff_bytes: 64 * 1024,
        }
    }
}
impl CheckpointLimits {
    fn validate(self) -> Result<(), BoundaryError> {
        if self.max_files == 0
            || self.max_files > 512
            || self.max_file_bytes == 0
            || self.max_file_bytes > 16 * 1024 * 1024
            || self.max_snapshot_bytes < self.max_file_bytes
            || self.max_snapshot_bytes > 64 * 1024 * 1024
            || self.max_checkpoints == 0
            || self.max_checkpoints > 1024
            || self.max_store_bytes < self.max_snapshot_bytes as u64
            || self.max_diff_bytes == 0
            || self.max_diff_bytes > 256 * 1024
        {
            return Err(error("checkpoint_config", "Checkpoint limits are invalid"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceIdentity {
    pub path: PathBuf,
    pub device: u64,
    pub inode: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    Directory,
    GitDirectory,
    GitWorktree,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointSource {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub revision: u64,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileState {
    Absent,
    File {
        sha256: String,
        bytes: u64,
        mode: u32,
    },
}
impl FileState {
    fn bytes(&self) -> u64 {
        match self {
            Self::Absent => 0,
            Self::File { bytes, .. } => *bytes,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileVersion {
    pub state: FileState,
    pub observation: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: String,
    state: FileState,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema_version: u32,
    id: String,
    parent: Option<CheckpointToken>,
    workspace: WorkspaceIdentity,
    workspace_kind: WorkspaceKind,
    source: CheckpointSource,
    files: Vec<Entry>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointToken {
    pub id: String,
    pub sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointSummary {
    pub token: CheckpointToken,
    pub parent: Option<CheckpointToken>,
    pub workspace: WorkspaceIdentity,
    pub workspace_kind: WorkspaceKind,
    pub source: CheckpointSource,
    pub files: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Unchanged,
}
fn change_kind(before: &FileState, after: &FileState) -> ChangeKind {
    match (before, after) {
        (a, b) if a == b => ChangeKind::Unchanged,
        (FileState::Absent, _) => ChangeKind::Added,
        (_, FileState::Absent) => ChangeKind::Deleted,
        _ => ChangeKind::Modified,
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    pub change: ChangeKind,
    pub baseline: FileState,
    pub current: FileVersion,
    pub unified_diff: Option<String>,
    pub preview_truncated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointDiff {
    pub checkpoint: CheckpointToken,
    pub source: CheckpointSource,
    pub files: Vec<FileDiff>,
    pub preview_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileObservation {
    pub path: String,
    pub current: FileVersion,
}
/// Host-created, single-use bytes and observations. Preparation performs no
/// snapshot publication. Authorization must bind these exact observations.
pub struct PreparedSnapshot {
    store: CheckpointStore,
    snapshot: Snapshot,
    contents: BTreeMap<String, Vec<u8>>,
    observations: Vec<FileObservation>,
}
impl std::fmt::Debug for PreparedSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedSnapshot")
            .field("files", &self.snapshot.files.len())
            .finish_non_exhaustive()
    }
}
impl PreparedSnapshot {
    pub fn observations(&self) -> &[FileObservation] {
        &self.observations
    }
    pub fn paths(&self) -> Vec<String> {
        self.snapshot
            .files
            .iter()
            .map(|entry| entry.path.clone())
            .collect()
    }
    pub fn source(&self) -> &CheckpointSource {
        &self.snapshot.source
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreOperation {
    pub path: String,
    pub expected: FileVersion,
    pub desired: FileState,
}
/// Constructed only by the bound workspace after reading all selected versions.
pub struct PreparedRestore {
    checkpoint: CheckpointToken,
    workspace: WorkspaceIdentity,
    source: CheckpointSource,
    operation_id: String,
    changes: Vec<RestoreOperation>,
    desired: BTreeMap<String, Vec<u8>>,
}
impl std::fmt::Debug for PreparedRestore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedRestore")
            .field("operation_id", &self.operation_id)
            .field("checkpoint", &self.checkpoint)
            .field("files", &self.changes.len())
            .finish_non_exhaustive()
    }
}
impl PreparedRestore {
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }
    pub fn changes(&self) -> &[RestoreOperation] {
        &self.changes
    }
    pub fn source(&self) -> &CheckpointSource {
        &self.source
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreStatus {
    Prepared,
    Applying,
    Completed,
    Cancelled,
    Rejected,
    Conflict,
    Partial,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOutcomeStatus {
    Pending,
    Unchanged,
    Applied,
    ObservedBefore,
    ObservedDesired,
    Rejected,
    Conflict,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileOutcome {
    pub operation: RestoreOperation,
    pub status: FileOutcomeStatus,
    pub publication_observed: bool,
    pub observed: Option<FileVersion>,
    pub error: Option<ErrorInfo>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangedFiles {
    pub schema_version: u32,
    pub operation_id: String,
    pub checkpoint: CheckpointToken,
    pub workspace: WorkspaceIdentity,
    pub source: CheckpointSource,
    pub status: RestoreStatus,
    pub files: Vec<FileOutcome>,
    pub error: Option<ErrorInfo>,
}
impl ChangedFiles {
    pub fn has_applied_files(&self) -> bool {
        self.files.iter().any(|file| file.publication_observed)
    }
}

struct Inner {
    disk: disk::Disk,
    secrets: Arc<SecretRegistry>,
    limits: CheckpointLimits,
}
#[derive(Clone)]
pub struct CheckpointStore {
    inner: Arc<Inner>,
}
impl std::fmt::Debug for CheckpointStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CheckpointStore([PRIVATE])")
    }
}
impl CheckpointStore {
    pub fn open(
        path: impl AsRef<Path>,
        secrets: Arc<SecretRegistry>,
        limits: CheckpointLimits,
    ) -> Result<Self, BoundaryError> {
        limits.validate()?;
        Ok(Self {
            inner: Arc::new(Inner {
                disk: disk::Disk::open(path.as_ref(), limits)?,
                secrets,
                limits,
            }),
        })
    }
    pub fn protected_path(&self) -> &Path {
        self.inner.disk.path()
    }
    pub fn limits(&self) -> CheckpointLimits {
        self.inner.limits
    }
    /// Only the trusted host chooses this root, session, and protected paths.
    pub fn bind_workspace(
        &self,
        path: &Path,
        session_id: SessionId,
        protected: &[PathBuf],
    ) -> Result<CheckpointWorkspace, BoundaryError> {
        let mut protected = protected.to_vec();
        protected.push(self.protected_path().to_owned());
        let workspace = workspace::Workspace::open(path, &protected)?;
        Ok(CheckpointWorkspace {
            store: self.clone(),
            workspace: Arc::new(workspace),
            session_id,
        })
    }
}
#[derive(Clone)]
pub struct CheckpointWorkspace {
    store: CheckpointStore,
    workspace: Arc<workspace::Workspace>,
    session_id: SessionId,
}
impl std::fmt::Debug for CheckpointWorkspace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CheckpointWorkspace([PRIVATE])")
    }
}
impl CheckpointWorkspace {
    pub fn identity(&self) -> &WorkspaceIdentity {
        &self.workspace.identity
    }
    pub fn kind(&self) -> WorkspaceKind {
        self.workspace.kind
    }

    pub fn capture(
        &self,
        paths: &[String],
        source: CheckpointSource,
        cancel: &AtomicBool,
    ) -> Result<CheckpointSummary, BoundaryError> {
        let prepared = self.prepare_capture(paths, source, cancel)?;
        self.publish_snapshot(prepared, cancel)
    }

    pub fn prepare_capture(
        &self,
        paths: &[String],
        source: CheckpointSource,
        cancel: &AtomicBool,
    ) -> Result<PreparedSnapshot, BoundaryError> {
        self.check_source(&source)?;
        let paths = self.paths(paths)?;
        let _lock = self.store.inner.disk.lock()?;
        self.workspace.check_identity()?;
        let mut contents = BTreeMap::new();
        let mut files = Vec::new();
        let mut observations = Vec::new();
        let mut total = 0u64;
        // Validate every file and secret before writing any snapshot bytes.
        for path in paths {
            let file = self.read(&path, cancel)?;
            self.check_content(&file.bytes)?;
            total = total
                .checked_add(file.version.state.bytes())
                .ok_or_else(|| error("checkpoint_limit", "Checkpoint byte count overflows"))?;
            if total > self.store.inner.limits.max_snapshot_bytes as u64 {
                return Err(error(
                    "checkpoint_limit",
                    "Checkpoint exceeds its byte limit",
                ));
            }
            contents.insert(path.clone(), file.bytes);
            observations.push(FileObservation {
                path: path.clone(),
                current: file.version.clone(),
            });
            files.push(Entry {
                path,
                state: file.version.state,
            });
        }
        let snapshot = Snapshot {
            schema_version: 1,
            id: Uuid::now_v7().to_string(),
            parent: None,
            workspace: self.identity().clone(),
            workspace_kind: self.kind(),
            source,
            files,
        };
        Ok(PreparedSnapshot {
            store: self.store.clone(),
            snapshot,
            contents,
            observations,
        })
    }

    /// Publish the captured approved bytes after checking that their source
    /// observations still match. A conflict creates no new baseline.
    pub fn publish_snapshot(
        &self,
        prepared: PreparedSnapshot,
        cancel: &AtomicBool,
    ) -> Result<CheckpointSummary, BoundaryError> {
        self.check_source(&prepared.snapshot.source)?;
        if !Arc::ptr_eq(&prepared.store.inner, &self.store.inner)
            || prepared.snapshot.workspace != *self.identity()
            || prepared.snapshot.workspace_kind != self.kind()
        {
            return Err(error(
                "checkpoint_conflict",
                "Prepared snapshot belongs to another workspace",
            ));
        }
        let _lock = self.store.inner.disk.lock()?;
        self.workspace.check_identity()?;
        for observation in &prepared.observations {
            let current = self.read(&observation.path, cancel)?;
            self.check_content(&current.bytes)?;
            if current.version != observation.current {
                return Err(error(
                    "checkpoint_conflict",
                    "Checkpoint source changed after preparation",
                ));
            }
        }
        for bytes in prepared.contents.values() {
            self.check_content(bytes)?;
        }
        self.persist(&prepared.snapshot, &prepared.contents, cancel)
    }

    pub fn list(&self) -> Result<Vec<CheckpointSummary>, BoundaryError> {
        let _lock = self.store.inner.disk.lock()?;
        self.workspace.check_identity()?;
        let mut summaries = Vec::new();
        for snapshot in self
            .store
            .inner
            .disk
            .snapshots(self.store.inner.limits.max_checkpoints)?
        {
            if snapshot.workspace == *self.identity()
                && snapshot.source.session_id == self.session_id
            {
                summaries.push(self.summary(&snapshot)?);
            }
        }
        Ok(summaries)
    }

    pub fn diff(
        &self,
        checkpoint: &CheckpointToken,
        cancel: &AtomicBool,
    ) -> Result<CheckpointDiff, BoundaryError> {
        let _lock = self.store.inner.disk.lock()?;
        let snapshot = self.load(checkpoint)?;
        let mut remaining = self.store.inner.limits.max_diff_bytes;
        let mut files = Vec::new();
        for entry in &snapshot.files {
            let current = self.read(&entry.path, cancel)?;
            let baseline = self.blob(&entry.state)?;
            let change = change_kind(&entry.state, &current.version.state);
            let (unified_diff, preview_truncated) =
                self.render_diff(&baseline, &current.bytes, change, &mut remaining)?;
            files.push(FileDiff {
                path: entry.path.clone(),
                change,
                baseline: entry.state.clone(),
                current: current.version,
                unified_diff,
                preview_truncated,
            });
        }
        let preview_truncated = files.iter().any(|file| file.preview_truncated);
        Ok(CheckpointDiff {
            checkpoint: checkpoint.clone(),
            source: snapshot.source,
            files,
            preview_truncated,
        })
    }

    /// Accept selected current files into a new immutable baseline. Unselected
    /// entries retain the previous checkpoint's bytes and provenance link.
    pub fn accept(
        &self,
        checkpoint: &CheckpointToken,
        paths: &[String],
        source: CheckpointSource,
        cancel: &AtomicBool,
    ) -> Result<CheckpointSummary, BoundaryError> {
        let prepared = self.prepare_accept(checkpoint, paths, source, cancel)?;
        self.publish_snapshot(prepared, cancel)
    }

    pub fn prepare_accept(
        &self,
        checkpoint: &CheckpointToken,
        paths: &[String],
        source: CheckpointSource,
        cancel: &AtomicBool,
    ) -> Result<PreparedSnapshot, BoundaryError> {
        self.check_source(&source)?;
        let selected: BTreeSet<_> = self.paths(paths)?.into_iter().collect();
        let _lock = self.store.inner.disk.lock()?;
        let previous = self.load(checkpoint)?;
        if selected
            .iter()
            .any(|path| !previous.files.iter().any(|entry| &entry.path == path))
        {
            return Err(error(
                "checkpoint_path",
                "Accepted paths must belong to the checkpoint",
            ));
        }
        let mut files = Vec::new();
        let mut contents = BTreeMap::new();
        let mut observations = Vec::new();
        for entry in previous.files {
            let (state, bytes) = if selected.contains(&entry.path) {
                let current = self.read(&entry.path, cancel)?;
                observations.push(FileObservation {
                    path: entry.path.clone(),
                    current: current.version.clone(),
                });
                (current.version.state, current.bytes)
            } else {
                (entry.state.clone(), self.blob(&entry.state)?)
            };
            self.check_content(&bytes)?;
            contents.insert(entry.path.clone(), bytes);
            files.push(Entry {
                path: entry.path,
                state,
            });
        }
        let snapshot = Snapshot {
            schema_version: 1,
            id: Uuid::now_v7().to_string(),
            parent: Some(checkpoint.clone()),
            workspace: self.identity().clone(),
            workspace_kind: self.kind(),
            source,
            files,
        };
        Ok(PreparedSnapshot {
            store: self.store.clone(),
            snapshot,
            contents,
            observations,
        })
    }

    pub fn prepare_restore(
        &self,
        checkpoint: &CheckpointToken,
        paths: &[String],
        source: CheckpointSource,
        cancel: &AtomicBool,
    ) -> Result<PreparedRestore, BoundaryError> {
        self.check_source(&source)?;
        let selected = self.paths(paths)?;
        let _lock = self.store.inner.disk.lock()?;
        let snapshot = self.load(checkpoint)?;
        let mut changes = Vec::new();
        let mut desired = BTreeMap::new();
        for path in selected {
            let entry = snapshot
                .files
                .iter()
                .find(|entry| entry.path == path)
                .ok_or_else(|| {
                    error(
                        "checkpoint_path",
                        "Restore paths must belong to the checkpoint",
                    )
                })?;
            let current = self.read(&path, cancel)?;
            let bytes = self.blob(&entry.state)?;
            self.check_content(&current.bytes)?;
            self.check_content(&bytes)?;
            changes.push(RestoreOperation {
                path: path.clone(),
                expected: current.version,
                desired: entry.state.clone(),
            });
            desired.insert(path, bytes);
        }
        Ok(PreparedRestore {
            checkpoint: checkpoint.clone(),
            workspace: self.identity().clone(),
            source,
            operation_id: Uuid::now_v7().to_string(),
            changes,
            desired,
        })
    }

    /// Restore once, recording intent and each observed result. The host checks
    /// current mode, scope, approval, and deny rules in `authorize` immediately
    /// before every changed file. This callback never receives snapshot bytes.
    pub fn apply(
        &self,
        prepared: &PreparedRestore,
        cancel: &AtomicBool,
        mut authorize: impl FnMut(&RestoreOperation) -> Result<(), BoundaryError>,
    ) -> Result<ChangedFiles, BoundaryError> {
        cancelled(cancel)?;
        self.check_source(&prepared.source)?;
        let _lock = self.store.inner.disk.lock()?;
        let snapshot = self.load(&prepared.checkpoint)?;
        if prepared.workspace != *self.identity()
            || prepared.changes.is_empty()
            || !valid_id(&prepared.operation_id)
        {
            return Err(error(
                "checkpoint_conflict",
                "Prepared restore belongs to another workspace",
            ));
        }
        for change in &prepared.changes {
            if !snapshot
                .files
                .iter()
                .any(|entry| entry.path == change.path && entry.state == change.desired)
            {
                return Err(error(
                    "checkpoint_conflict",
                    "Prepared restore differs from the immutable checkpoint",
                ));
            }
            self.check_content(prepared.desired.get(&change.path).ok_or_else(|| {
                error("checkpoint_corrupt", "Prepared restore bytes are missing")
            })?)?;
        }
        if let Some(existing) = self.store.inner.disk.operation(&prepared.operation_id)? {
            self.validate_outcome(&existing)?;
            if existing.checkpoint != prepared.checkpoint
                || existing.source != prepared.source
                || existing.workspace != prepared.workspace
                || existing
                    .files
                    .iter()
                    .map(|file| &file.operation)
                    .collect::<Vec<_>>()
                    != prepared.changes.iter().collect::<Vec<_>>()
            {
                return Err(error(
                    "checkpoint_conflict",
                    "Restore operation identity was reused",
                ));
            }
            if matches!(
                existing.status,
                RestoreStatus::Prepared | RestoreStatus::Applying | RestoreStatus::Unknown
            ) {
                return Err(error(
                    "checkpoint_pending",
                    "Restore outcome requires observation; it cannot be replayed",
                ));
            }
            return Ok(existing);
        }
        let mut outcome = ChangedFiles {
            schema_version: 1,
            operation_id: prepared.operation_id.clone(),
            checkpoint: prepared.checkpoint.clone(),
            workspace: prepared.workspace.clone(),
            source: prepared.source.clone(),
            status: RestoreStatus::Prepared,
            files: prepared
                .changes
                .iter()
                .cloned()
                .map(|operation| FileOutcome {
                    operation,
                    status: FileOutcomeStatus::Pending,
                    publication_observed: false,
                    observed: None,
                    error: None,
                })
                .collect(),
            error: None,
        };
        // All selected paths are checked before the first workspace mutation.
        for index in 0..outcome.files.len() {
            match self.read(&outcome.files[index].operation.path, cancel) {
                Ok(current) if current.version == outcome.files[index].operation.expected => {
                    self.check_content(&current.bytes)?
                }
                Ok(current) => {
                    outcome.files[index].observed = Some(current.version);
                    outcome.files[index].status = FileOutcomeStatus::Conflict;
                    outcome.status = RestoreStatus::Conflict;
                    outcome.error = Some(ErrorInfo {
                        code: "checkpoint_conflict".into(),
                        message: "Selected files changed after restore preparation".into(),
                    });
                    self.save_outcome(&outcome)?;
                    return Ok(outcome);
                }
                Err(failure) => {
                    outcome.status = if failure.0.code == "cancelled" {
                        RestoreStatus::Cancelled
                    } else {
                        RestoreStatus::Conflict
                    };
                    outcome.error = Some(failure.0);
                    self.save_outcome(&outcome)?;
                    return Ok(outcome);
                }
            }
        }
        self.save_outcome(&outcome)?;
        for index in 0..outcome.files.len() {
            if let Err(failure) = cancelled(cancel) {
                outcome.status = if outcome.has_applied_files() {
                    RestoreStatus::Partial
                } else {
                    RestoreStatus::Cancelled
                };
                outcome.error = Some(failure.0);
                self.save_outcome(&outcome)?;
                return Ok(outcome);
            }
            let change = &prepared.changes[index];
            if change.expected.state == change.desired {
                outcome.files[index].status = FileOutcomeStatus::Unchanged;
                outcome.files[index].observed = Some(change.expected.clone());
                self.save_outcome(&outcome)?;
                continue;
            }
            if authorize(change).is_err() {
                outcome.files[index].status = FileOutcomeStatus::Rejected;
                outcome.status = if outcome.has_applied_files() {
                    RestoreStatus::Partial
                } else {
                    RestoreStatus::Rejected
                };
                outcome.error = Some(ErrorInfo {
                    code: "permission_denied".into(),
                    message: "Restore authorization was denied or became stale".into(),
                });
                self.save_outcome(&outcome)?;
                return Ok(outcome);
            }
            let content_check = self
                .check_content(&prepared.desired[&change.path])
                .and_then(|_| self.read(&change.path, cancel))
                .and_then(|current| self.check_content(&current.bytes));
            if let Err(failure) = content_check {
                outcome.status = if outcome.has_applied_files() {
                    RestoreStatus::Partial
                } else if failure.0.code == "cancelled" {
                    RestoreStatus::Cancelled
                } else {
                    RestoreStatus::Rejected
                };
                outcome.error = Some(failure.0);
                self.save_outcome(&outcome)?;
                return Ok(outcome);
            }
            outcome.status = RestoreStatus::Applying;
            outcome.files[index].status = FileOutcomeStatus::Unknown;
            self.save_outcome(&outcome)?;
            let result = self.workspace.publish(
                change,
                &prepared.desired[&change.path],
                self.store.inner.limits.max_file_bytes,
                cancel,
            );
            outcome.files[index].publication_observed = result.is_ok();
            let observed = self
                .read(&change.path, &AtomicBool::new(false))
                .ok()
                .map(|file| file.version);
            outcome.files[index].observed = observed.clone();
            match result {
                Ok(())
                    if observed
                        .as_ref()
                        .is_some_and(|version| version.state == change.desired) =>
                {
                    outcome.files[index].status = FileOutcomeStatus::Applied
                }
                failure => {
                    let failure = failure.err().unwrap_or_else(|| {
                        error(
                            "checkpoint_unknown",
                            "File changed after restore publication",
                        )
                    });
                    outcome.files[index].status = if failure.0.code == "checkpoint_conflict" {
                        FileOutcomeStatus::Conflict
                    } else {
                        FileOutcomeStatus::Unknown
                    };
                    outcome.files[index].error = Some(failure.0.clone());
                    outcome.status = if outcome.has_applied_files() {
                        RestoreStatus::Partial
                    } else if failure.0.code == "checkpoint_conflict" {
                        RestoreStatus::Conflict
                    } else {
                        RestoreStatus::Unknown
                    };
                    outcome.error = Some(failure.0);
                    self.save_outcome(&outcome)?;
                    return Ok(outcome);
                }
            }
            self.save_outcome(&outcome)?;
        }
        // This final observation catches changes to an earlier file while later
        // files were being restored. It remains a sequence of observations.
        for index in 0..outcome.files.len() {
            match self.read(&outcome.files[index].operation.path, cancel) {
                Ok(current) if current.version.state == outcome.files[index].operation.desired => {
                    outcome.files[index].observed = Some(current.version)
                }
                observation => {
                    let cancelled = observation
                        .as_ref()
                        .is_err_and(|failure| failure.0.code == "cancelled");
                    outcome.status = if outcome.has_applied_files() {
                        RestoreStatus::Partial
                    } else if cancelled {
                        RestoreStatus::Cancelled
                    } else {
                        RestoreStatus::Conflict
                    };
                    outcome.files[index].status = if cancelled {
                        FileOutcomeStatus::Unknown
                    } else {
                        FileOutcomeStatus::Conflict
                    };
                    outcome.files[index].observed = observation.ok().map(|current| current.version);
                    outcome.error = Some(if cancelled {
                        ErrorInfo {
                            code: "cancelled".into(),
                            message: "Restore was cancelled during final observation".into(),
                        }
                    } else {
                        ErrorInfo {
                            code: "checkpoint_conflict".into(),
                            message: "Files changed during final restore observation".into(),
                        }
                    });
                    self.save_outcome(&outcome)?;
                    return Ok(outcome);
                }
            }
        }
        outcome.status = RestoreStatus::Completed;
        self.save_outcome(&outcome)?;
        Ok(outcome)
    }

    /// Observe interrupted intents without performing workspace writes. Seeing
    /// desired bytes is evidence of current state, not attribution to our write.
    pub fn recover(&self, cancel: &AtomicBool) -> Result<Vec<ChangedFiles>, BoundaryError> {
        let _lock = self.store.inner.disk.lock()?;
        self.workspace.check_identity()?;
        let mut recovered = Vec::new();
        for mut outcome in self
            .store
            .inner
            .disk
            .operations(self.store.inner.limits.max_checkpoints.saturating_mul(4))?
        {
            if outcome.workspace != *self.identity() || outcome.source.session_id != self.session_id
            {
                continue;
            }
            self.validate_outcome(&outcome)?;
            if !matches!(
                outcome.status,
                RestoreStatus::Prepared | RestoreStatus::Applying | RestoreStatus::Unknown
            ) {
                continue;
            }
            for file in &mut outcome.files {
                cancelled(cancel)?;
                let observed = self
                    .read(&file.operation.path, cancel)
                    .ok()
                    .map(|file| file.version);
                file.status = match &observed {
                    Some(version) if *version == file.operation.expected => {
                        FileOutcomeStatus::ObservedBefore
                    }
                    Some(version) if version.state == file.operation.desired => {
                        FileOutcomeStatus::ObservedDesired
                    }
                    _ => FileOutcomeStatus::Unknown,
                };
                file.observed = observed;
            }
            outcome.status = RestoreStatus::Unknown;
            outcome.error = Some(ErrorInfo {
                code: "checkpoint_unknown".into(),
                message: "Interrupted restore was observed and has not been replayed".into(),
            });
            self.save_outcome(&outcome)?;
            recovered.push(outcome);
        }
        self.store.inner.disk.collect_unreferenced_blobs()?;
        Ok(recovered)
    }

    fn check_source(&self, source: &CheckpointSource) -> Result<(), BoundaryError> {
        if source.session_id != self.session_id
            || source.label.is_empty()
            || source.label.len() > 256
            || source.label.chars().any(crate::text::is_terminal_control)
        {
            return Err(error(
                "checkpoint_scope",
                "Checkpoint source is invalid or belongs to another session",
            ));
        }
        self.check_metadata(&(source, self.identity()))
    }
    fn paths(&self, paths: &[String]) -> Result<Vec<String>, BoundaryError> {
        if paths.is_empty() || paths.len() > self.store.inner.limits.max_files {
            return Err(error(
                "checkpoint_limit",
                "Checkpoint selected-path count exceeds its limit",
            ));
        }
        let mut selected = BTreeSet::new();
        let mut budget = crate::plugin_manifest::FileBudget::default();
        for path in paths {
            self.workspace.validate_path(path)?;
            budget.reserve_file(path, 0)?;
            self.check_metadata(path)?;
            if !selected.insert(path.clone()) {
                return Err(error(
                    "checkpoint_path",
                    "Checkpoint selected paths are duplicated",
                ));
            }
        }
        Ok(selected.into_iter().collect())
    }
    fn read(&self, path: &str, cancel: &AtomicBool) -> Result<workspace::FileRead, BoundaryError> {
        self.workspace
            .read(path, self.store.inner.limits.max_file_bytes, cancel)
    }
    fn check_content(&self, bytes: &[u8]) -> Result<(), BoundaryError> {
        let text = std::str::from_utf8(bytes).map_err(|_| {
            error(
                "checkpoint_unsupported",
                "Opaque or non-UTF-8 checkpoint files are unsupported",
            )
        })?;
        if bytes.contains(&0) {
            return Err(error(
                "checkpoint_unsupported",
                "Opaque checkpoint files are unsupported",
            ));
        }
        if crate::redaction::contains_secret(text, &self.store.inner.secrets.snapshot()?) {
            return Err(error(
                "sensitive_data",
                "Checkpoint material contains protected data",
            ));
        }
        Ok(())
    }
    fn check_metadata<T: Serialize + ?Sized>(&self, value: &T) -> Result<(), BoundaryError> {
        let encoded = serde_json::to_vec(value).map_err(|_| {
            error(
                "checkpoint_corrupt",
                "Checkpoint metadata cannot be encoded",
            )
        })?;
        self.check_content(&encoded)
    }
    fn blob(&self, state: &FileState) -> Result<Vec<u8>, BoundaryError> {
        match state {
            FileState::Absent => Ok(Vec::new()),
            FileState::File { sha256, bytes, .. } => {
                self.store
                    .inner
                    .disk
                    .blob(sha256, *bytes, self.store.inner.limits.max_file_bytes)
            }
        }
    }
    fn load(&self, token: &CheckpointToken) -> Result<Snapshot, BoundaryError> {
        self.workspace.check_identity()?;
        let snapshot = self.store.inner.disk.snapshot(token)?;
        self.validate_snapshot(&snapshot)?;
        Ok(snapshot)
    }
    fn validate_snapshot(&self, snapshot: &Snapshot) -> Result<(), BoundaryError> {
        self.check_source(&snapshot.source)?;
        if snapshot.schema_version != 1
            || !valid_id(&snapshot.id)
            || snapshot.workspace != *self.identity()
            || snapshot.workspace_kind != self.kind()
            || snapshot
                .parent
                .as_ref()
                .is_some_and(|token| !valid_id(&token.id) || !valid_hash(&token.sha256))
        {
            return Err(error(
                "checkpoint_corrupt",
                "Checkpoint identity is invalid",
            ));
        }
        self.paths(
            &snapshot
                .files
                .iter()
                .map(|entry| entry.path.clone())
                .collect::<Vec<_>>(),
        )?;
        let mut total = 0u64;
        for entry in &snapshot.files {
            validate_state(&entry.state, self.store.inner.limits)?;
            total = total
                .checked_add(entry.state.bytes())
                .ok_or_else(|| error("checkpoint_limit", "Checkpoint sizes overflow"))?;
        }
        if total > self.store.inner.limits.max_snapshot_bytes as u64 {
            return Err(error(
                "checkpoint_limit",
                "Checkpoint total bytes exceed its limit",
            ));
        }
        Ok(())
    }
    fn summary(&self, snapshot: &Snapshot) -> Result<CheckpointSummary, BoundaryError> {
        self.validate_snapshot(snapshot)?;
        let token = CheckpointToken {
            id: snapshot.id.clone(),
            sha256: hash(
                &serde_json::to_vec(snapshot)
                    .map_err(|_| error("checkpoint_corrupt", "Checkpoint cannot be encoded"))?,
            ),
        };
        Ok(CheckpointSummary {
            token,
            parent: snapshot.parent.clone(),
            workspace: snapshot.workspace.clone(),
            workspace_kind: snapshot.workspace_kind,
            source: snapshot.source.clone(),
            files: snapshot.files.len(),
            bytes: snapshot.files.iter().map(|entry| entry.state.bytes()).sum(),
        })
    }
    fn persist(
        &self,
        snapshot: &Snapshot,
        contents: &BTreeMap<String, Vec<u8>>,
        cancel: &AtomicBool,
    ) -> Result<CheckpointSummary, BoundaryError> {
        self.validate_snapshot(snapshot)?;
        self.check_metadata(snapshot)?;
        for entry in &snapshot.files {
            let bytes = contents.get(&entry.path).ok_or_else(|| {
                error("checkpoint_corrupt", "Checkpoint source bytes are missing")
            })?;
            self.check_content(bytes)?;
            if let FileState::File {
                sha256,
                bytes: length,
                ..
            } = &entry.state
                && (hash(bytes) != *sha256 || bytes.len() as u64 != *length)
            {
                return Err(error(
                    "checkpoint_corrupt",
                    "Checkpoint source bytes differ from their version",
                ));
            }
        }
        cancelled(cancel)?;
        self.workspace.check_identity()?;
        self.store
            .inner
            .disk
            .check_capacity(snapshot, contents, self.store.inner.limits)?;
        for entry in &snapshot.files {
            cancelled(cancel)?;
            self.check_content(&contents[&entry.path])?;
            if let FileState::File { sha256, .. } = &entry.state {
                self.store
                    .inner
                    .disk
                    .save_blob(sha256, &contents[&entry.path])?;
            }
        }
        cancelled(cancel)?;
        for bytes in contents.values() {
            self.check_content(bytes)?;
        }
        self.store.inner.disk.save_snapshot(snapshot)?;
        self.summary(snapshot)
    }
    fn validate_outcome(&self, outcome: &ChangedFiles) -> Result<(), BoundaryError> {
        self.check_source(&outcome.source)?;
        if outcome.schema_version != 1
            || !valid_id(&outcome.operation_id)
            || outcome.workspace != *self.identity()
            || !valid_id(&outcome.checkpoint.id)
            || !valid_hash(&outcome.checkpoint.sha256)
        {
            return Err(error(
                "checkpoint_corrupt",
                "Restore journal identity is invalid",
            ));
        }
        self.paths(
            &outcome
                .files
                .iter()
                .map(|file| file.operation.path.clone())
                .collect::<Vec<_>>(),
        )?;
        let snapshot = self.load(&outcome.checkpoint)?;
        for file in &outcome.files {
            validate_state(&file.operation.expected.state, self.store.inner.limits)?;
            validate_state(&file.operation.desired, self.store.inner.limits)?;
            if !snapshot.files.iter().any(|entry| {
                entry.path == file.operation.path && entry.state == file.operation.desired
            }) {
                return Err(error(
                    "checkpoint_corrupt",
                    "Restore journal differs from its checkpoint source",
                ));
            }
            if let Some(observed) = &file.observed {
                validate_state(&observed.state, self.store.inner.limits)?;
            }
            if !valid_hash(&file.operation.expected.observation)
                || file
                    .observed
                    .as_ref()
                    .is_some_and(|version| !valid_hash(&version.observation))
            {
                return Err(error(
                    "checkpoint_corrupt",
                    "Restore journal version is invalid",
                ));
            }
        }
        self.check_metadata(outcome)
    }
    fn save_outcome(&self, outcome: &ChangedFiles) -> Result<(), BoundaryError> {
        self.validate_outcome(outcome)
            .and_then(|_| self.store.inner.disk.save_operation(outcome))
            .map_err(|_| {
                let mut failure = error(
                    "checkpoint_storage",
                    "Restore journal durability is unknown; inspect observed file evidence",
                );
                let data = serde_json::to_value(outcome).ok().and_then(|mut value| {
                    let secrets = self.store.inner.secrets.snapshot().ok()?;
                    crate::redaction::redact(&mut value, &secrets).ok()?;
                    let encoded = serde_json::to_string(&value).ok()?;
                    (!crate::redaction::contains_secret(&encoded, &secrets)).then_some(value)
                });
                let truncated = data.is_none();
                failure.2 = Some(Box::new(crate::contract::ToolResult {
                    ok: false,
                    data,
                    error: Some(failure.0.clone()),
                    truncated,
                }));
                failure
            })
    }
    fn render_diff(
        &self,
        before: &[u8],
        after: &[u8],
        change: ChangeKind,
        remaining: &mut usize,
    ) -> Result<(Option<String>, bool), BoundaryError> {
        if change == ChangeKind::Unchanged || before == after {
            return Ok((None, false));
        }
        if *remaining == 0
            || before.len() + after.len() > 128 * 1024
            || before.iter().filter(|byte| **byte == b'\n').count() > 2000
            || after.iter().filter(|byte| **byte == b'\n').count() > 2000
        {
            return Ok((None, true));
        }
        let before = std::str::from_utf8(before).map_err(|_| {
            error(
                "checkpoint_unsupported",
                "Opaque checkpoint diff is unsupported",
            )
        })?;
        let after = std::str::from_utf8(after).map_err(|_| {
            error(
                "checkpoint_unsupported",
                "Opaque checkpoint diff is unsupported",
            )
        })?;
        let patch = diffy::create_patch(before, after).to_string();
        let mut value = serde_json::Value::String(patch);
        crate::redaction::redact(&mut value, &self.store.inner.secrets.snapshot()?).map_err(
            |_| {
                error(
                    "sensitive_data",
                    "Checkpoint diff cannot be safely redacted",
                )
            },
        )?;
        let serde_json::Value::String(text) = value else {
            return Err(error(
                "checkpoint_corrupt",
                "Checkpoint diff encoding is invalid",
            ));
        };
        let mut end = text.len().min(*remaining);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let truncated = end < text.len();
        *remaining -= end;
        Ok((Some(text[..end].to_owned()), truncated))
    }
}

fn validate_state(state: &FileState, limits: CheckpointLimits) -> Result<(), BoundaryError> {
    if let FileState::File {
        sha256,
        bytes,
        mode,
    } = state
        && (!valid_hash(sha256) || *bytes > limits.max_file_bytes as u64 || *mode & !0o777 != 0)
    {
        return Err(error(
            "checkpoint_corrupt",
            "Checkpoint file state is invalid",
        ));
    }
    Ok(())
}
