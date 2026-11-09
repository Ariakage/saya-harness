use super::*;
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
use state::{JOURNAL_PATH, Ledger, STAGE_PATH, STATE_PATH};
use std::{
    fs::{DirBuilder, File},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt},
    },
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCommitStatus {
    Committed,
    AlreadyCommitted,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryCommitOutcome {
    pub status: MemoryCommitStatus,
    pub receipt: MemoryOperationReceipt,
    pub revision: Option<String>,
    pub error: Option<ErrorInfo>,
    pub historical_copies_retained: bool,
    pub projection_refresh_required: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema_version: u32,
    before_index_digest: Option<String>,
    after_index_digest: String,
    ledger: Ledger,
    receipt: MemoryOperationReceipt,
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FaultPoint {
    None,
    AfterStage,
    AfterJournal,
    AfterIndex,
    AfterLedger,
}
#[cfg(test)]
thread_local! { static FAULT: std::cell::Cell<FaultPoint> = const { std::cell::Cell::new(FaultPoint::None) }; }
#[cfg(test)]
pub(super) fn fail_once(point: FaultPoint) {
    FAULT.set(point);
}
#[cfg(test)]
fn checkpoint(point: FaultPoint) -> Result<(), BoundaryError> {
    if FAULT.get() == point {
        FAULT.set(FaultPoint::None);
        return Err(error("memory_io", "Injected memory publication failure"));
    }
    Ok(())
}

struct Lock {
    _file: File,
}
fn cancelled(cancel: &AtomicBool) -> Result<(), BoundaryError> {
    if cancel.load(Ordering::Acquire) {
        return Err(error("cancelled", "Memory operation was cancelled"));
    }
    Ok(())
}
fn sync_directory(dir: &Dir) -> Result<(), BoundaryError> {
    dir.try_clone()
        .map_err(|_| error("memory_io", "Memory directory cannot be cloned"))?
        .into_std_file()
        .sync_all()
        .map_err(|_| {
            error(
                "memory_io",
                "Memory directory durability could not be confirmed",
            )
        })
}
fn directory(root: &disk::MemoryRoot, relative: &str, create: bool) -> Result<Dir, BoundaryError> {
    let mut parent = root
        .dir
        .try_clone()
        .map_err(|_| error("memory_io", "Memory directory cannot be cloned"))?;
    for component in relative.split('/').filter(|part| !part.is_empty()) {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
        if create {
            let component_c = std::ffi::CString::new(component)
                .map_err(|_| error("memory_path", "Memory directory name is invalid"))?;
            // SAFETY: the descriptor is an owned directory and the component
            // is a NUL-terminated internal filename, never a supplied path.
            let result = unsafe { libc::mkdirat(parent.as_raw_fd(), component_c.as_ptr(), 0o700) };
            if result != 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(error("memory_io", "Memory directory cannot be created"));
            }
        }
        let file = parent
            .open_with(component, &options)
            .map_err(|_| error("memory_path", "Memory directory is unavailable or linked"))?
            .into_std();
        let metadata = file
            .metadata()
            .map_err(|_| error("memory_io", "Memory directory metadata is unavailable"))?;
        // SAFETY: geteuid has no pointer arguments or ownership requirements.
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o022 != 0
        {
            return Err(error(
                "memory_permissions",
                "Memory directory is not owned and protected",
            ));
        }
        if create {
            sync_directory(&parent)?;
        }
        parent = Dir::from_std_file(file);
    }
    Ok(parent)
}
fn split_path<'a>(root: &disk::MemoryRoot, path: &'a str) -> Result<(Dir, &'a str), BoundaryError> {
    let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
    Ok((directory(root, parent, false)?, name))
}
fn atomic_write(root: &disk::MemoryRoot, path: &str, bytes: &[u8]) -> Result<(), BoundaryError> {
    let (parent, name) = split_path(root, path)?;
    let temporary = format!(".saya-write-{}.tmp", uuid::Uuid::now_v7().simple());
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let outcome = (|| {
        let mut file = parent
            .open_with(&temporary, &options)
            .map_err(|_| error("memory_io", "Memory temporary file cannot be created"))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| error("memory_io", "Memory temporary file cannot be saved"))?;
        parent
            .rename(&temporary, &parent, name)
            .map_err(|_| error("memory_io", "Memory atomic replacement failed"))?;
        sync_directory(&parent)
    })();
    if outcome.is_err() {
        let _ = parent.remove_file(&temporary);
    }
    outcome
}
fn remove(root: &disk::MemoryRoot, path: &str) -> Result<(), BoundaryError> {
    let (parent, name) = split_path(root, path)?;
    match parent.remove_file(name) {
        Ok(()) => sync_directory(&parent),
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(error(
            "memory_io",
            "Memory transaction file cannot be removed",
        )),
    }
}

fn acquire(root: &disk::MemoryRoot, cancel: &AtomicBool) -> Result<Lock, BoundaryError> {
    cancelled(cancel)?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let file = root
        .dir
        .open_with(".saya-memory.lock", &options)
        .map_err(|_| error("memory_path", "Memory lock is unavailable or linked"))?
        .into_std();
    let metadata = file
        .metadata()
        .map_err(|_| error("memory_io", "Memory lock metadata is unavailable"))?;
    // SAFETY: geteuid has no pointer arguments or ownership requirements.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
    {
        return Err(error(
            "memory_permissions",
            "Memory lock must be owned and private",
        ));
    }
    let deadline = Instant::now() + Duration::from_millis(200);
    loop {
        cancelled(cancel)?;
        // SAFETY: flock is applied to the live owned regular-file descriptor.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Lock { _file: file });
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::WouldBlock {
            return Err(error("memory_io", "Memory lock cannot be acquired"));
        }
        if Instant::now() >= deadline {
            return Err(error(
                "memory_busy",
                "Memory store is being changed by another writer",
            ));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn open_write(store: &GlobalMemoryStore) -> Result<disk::MemoryRoot, BoundaryError> {
    match DirBuilder::new().mode(0o700).create(&store.root) {
        Ok(()) => {}
        Err(failure) if failure.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(error("memory_io", "Memory root cannot be created")),
    }
    disk::open_root(&store.root)?
        .ok_or_else(|| error("memory_io", "Memory root disappeared while opening"))
}
fn verify_root(root: &disk::MemoryRoot, store: &GlobalMemoryStore) -> Result<(), BoundaryError> {
    let named = disk::open_root(&store.root)?
        .ok_or_else(|| error("memory_conflict", "Memory root disappeared during a change"))?;
    let original = root
        .dir
        .try_clone()
        .map_err(|_| error("memory_io", "Memory root cannot be cloned"))?
        .into_std_file()
        .metadata()
        .map_err(|_| error("memory_io", "Memory root metadata is unavailable"))?;
    let current = named
        .dir
        .into_std_file()
        .metadata()
        .map_err(|_| error("memory_io", "Memory root metadata is unavailable"))?;
    if original.dev() != current.dev() || original.ino() != current.ino() {
        return Err(error(
            "memory_conflict",
            "Memory root changed during a change",
        ));
    }
    Ok(())
}
fn encode<T: Serialize>(value: &T, max: usize) -> Result<Vec<u8>, BoundaryError> {
    encode_bounded(value, max)
}
fn history_path(version: &str) -> Result<String, BoundaryError> {
    if !valid_digest(version) {
        return Err(error("memory_args", "Memory history version is invalid"));
    }
    Ok(format!("memory/.saya-history/{version}.md"))
}
fn temporary_name(name: &str) -> bool {
    name.strip_prefix(".saya-write-")
        .and_then(|name| name.strip_suffix(".tmp"))
        .is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
}
fn clean_unjournaled(
    root: &disk::MemoryRoot,
    ledger: &Ledger,
    cancel: &AtomicBool,
) -> Result<(), BoundaryError> {
    if root.file(STAGE_PATH)?.is_some() {
        remove(root, STAGE_PATH)?;
    }
    for relative in ["", "memory", "memory/.saya-history"] {
        let parent = directory(root, relative, false)?;
        let entries = parent
            .entries()
            .map_err(|_| error("memory_io", "Memory private directory cannot be listed"))?;
        for entry in entries.take(8192) {
            cancelled(cancel)?;
            let entry = entry
                .map_err(|_| error("memory_io", "Memory private directory entry cannot be read"))?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let orphan_history = relative == "memory/.saya-history"
                && name.strip_suffix(".md").is_some_and(|version| {
                    valid_digest(version)
                        && !ledger
                            .history
                            .iter()
                            .any(|retained| retained.version == version)
                });
            if !temporary_name(&name) && !orphan_history {
                continue;
            }
            let path = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            // Check ownership, type, link count and privacy before unlinking a
            // recognized abandoned internal object. Other files are preserved.
            if root.file(&path)?.is_some() {
                remove(root, &path)?;
            }
        }
    }
    Ok(())
}

fn journal(
    root: &disk::MemoryRoot,
    store: &GlobalMemoryStore,
) -> Result<Option<Journal>, BoundaryError> {
    let Some(raw) = root.read(JOURNAL_PATH, state::max_state_bytes(store.limits))? else {
        return Ok(None);
    };
    let secrets = store.secrets.snapshot()?;
    if crate::redaction::contains_secret(
        std::str::from_utf8(&raw)
            .map_err(|_| error("memory_state", "Memory recovery record must be UTF-8"))?,
        &secrets,
    ) {
        return Err(error(
            "sensitive_data",
            "Memory recovery record contains protected data",
        ));
    }
    let journal: Journal = serde_json::from_slice(&raw)
        .map_err(|_| error("memory_state", "Memory recovery record cannot be parsed"))?;
    journal.ledger.validate(store.limits, &secrets)?;
    if journal.schema_version != 1
        || !valid_digest(&journal.after_index_digest)
        || journal
            .before_index_digest
            .as_ref()
            .is_some_and(|value| !valid_digest(value))
        || journal.receipt.document_digest != journal.after_index_digest
        || journal.ledger.operations.last() != Some(&journal.receipt)
    {
        return Err(error(
            "memory_state",
            "Memory recovery record is inconsistent",
        ));
    }
    Ok(Some(journal))
}

/// Finish the irrevocable, journaled section. A failure leaves the journal for
/// explicit host recovery. It never retries an arbitrary model operation.
fn recover_locked(
    root: &disk::MemoryRoot,
    store: &GlobalMemoryStore,
) -> Result<Option<MemoryOperationReceipt>, BoundaryError> {
    let Some(journal) = journal(root, store)? else {
        return Ok(None);
    };
    let secrets = store.secrets.snapshot()?;
    let (current_ledger, _) = state::load(Some(root), store.limits, &secrets)?;
    if current_ledger.generation != journal.ledger.generation
        && current_ledger.generation.checked_add(1) != Some(journal.ledger.generation)
    {
        return Err(error(
            "memory_recovery_conflict",
            "Memory recovery generation conflicts with current state",
        ));
    }
    if (current_ledger.generation == journal.ledger.generation && current_ledger != journal.ledger)
        || (current_ledger.generation != journal.ledger.generation
            && journal
                .ledger
                .operations
                .get(..journal.ledger.operations.len().saturating_sub(1))
                != Some(current_ledger.operations.as_slice()))
    {
        return Err(error(
            "memory_recovery_conflict",
            "Memory recovery operation ledger conflicts with current state",
        ));
    }
    verify_root(root, store)?;
    let index = root.read("MEMORY.md", store.limits.max_index_bytes)?;
    let current_digest = index.as_ref().map(|bytes| digest(bytes));
    if current_digest.as_ref() != Some(&journal.after_index_digest) {
        if current_digest != journal.before_index_digest {
            return Err(error(
                "memory_recovery_conflict",
                "Memory was edited after an unfinished operation",
            ));
        }
        let staged = root
            .read(STAGE_PATH, store.limits.max_index_bytes)?
            .ok_or_else(|| error("memory_recovery_conflict", "Memory staged index is missing"))?;
        if digest(&staged) != journal.after_index_digest {
            return Err(error(
                "memory_state",
                "Memory staged index digest is invalid",
            ));
        }
        let _ = store.parse_document(
            std::str::from_utf8(&staged)
                .map_err(|_| error("memory_format", "Memory staged index must be UTF-8"))?,
        )?;
        let (source, name) = split_path(root, STAGE_PATH)?;
        source
            .rename(name, &root.dir, "MEMORY.md")
            .map_err(|_| error("memory_io", "Memory index cannot be published"))?;
        sync_directory(&source)?;
        sync_directory(&root.dir)?;
        #[cfg(test)]
        checkpoint(FaultPoint::AfterIndex)?;
    } else if let Some(index) = &index {
        let _ = store.parse_document(
            std::str::from_utf8(index)
                .map_err(|_| error("memory_format", "Memory index must be UTF-8"))?,
        )?;
    }
    atomic_write(
        root,
        STATE_PATH,
        &encode(&journal.ledger, state::max_state_bytes(store.limits))?,
    )?;
    #[cfg(test)]
    checkpoint(FaultPoint::AfterLedger)?;
    remove(root, STAGE_PATH)?;
    remove(root, JOURNAL_PATH)?;
    verify_root(root, store)?;
    Ok(Some(journal.receipt))
}

impl GlobalMemoryStore {
    /// Call only after the host has authorized this exact proposal. Blocking
    /// filesystem work should run on a bounded worker. Before the WAL is saved,
    /// cancellation is side-effect-free for active memory. After it is saved,
    /// the outcome is either committed or explicitly unknown pending recovery.
    pub fn commit(&self, proposal: &MemoryProposal) -> Result<MemoryCommitOutcome, BoundaryError> {
        self.commit_cancellable(proposal, &AtomicBool::new(false))
    }
    pub fn commit_cancellable(
        &self,
        proposal: &MemoryProposal,
        cancel: &AtomicBool,
    ) -> Result<MemoryCommitOutcome, BoundaryError> {
        cancelled(cancel)?;
        if proposal.store_binding != self.binding() {
            return Err(error(
                "memory_scope",
                "Memory proposal belongs to another configured store",
            ));
        }
        if digest(&encode_bounded(
            &proposal.change,
            state::max_state_bytes(self.limits),
        )?) != proposal.change_digest
        {
            return Err(error(
                "memory_conflict",
                "Memory proposal change identity is invalid",
            ));
        }
        let root = open_write(self)?;
        let _lock = acquire(&root, cancel)?;
        let _ = recover_locked(&root, self)?;
        let secrets = self.secrets.snapshot()?;
        let (mut ledger, _) = state::load(Some(&root), self.limits, &secrets)?;
        if let Some(receipt) = ledger
            .operations
            .iter()
            .find(|operation| operation.operation_id == proposal.operation_id)
        {
            let same_request = match (&receipt.host_request_digest, &proposal.host_request_digest) {
                (Some(previous), Some(current)) => previous == current,
                (None, None) => receipt.change_digest == proposal.change_digest,
                _ => false,
            };
            if !same_request {
                return Err(error(
                    "memory_idempotency_conflict",
                    "Memory operation identity was reused for a different change",
                ));
            }
            return Ok(self.outcome(
                MemoryCommitStatus::AlreadyCommitted,
                receipt.clone(),
                &ledger,
            ));
        }
        if ledger.operations.len() >= self.limits.max_facts.saturating_mul(4) {
            return Err(error("memory_limit", "Memory operation ledger is full"));
        }
        let verified = self.propose(
            &proposal.expected_revision,
            &proposal.operation_id,
            proposal.change.clone(),
        )?;
        if verified.change_digest != proposal.change_digest
            || verified.proposed_document_digest != proposal.proposed_document_digest
        {
            return Err(error(
                "memory_conflict",
                "Memory proposal no longer matches its reviewed content",
            ));
        }
        cancelled(cancel)?;
        let rendered = self.render_index(&verified.document)?;
        let before = root.read("MEMORY.md", self.limits.max_index_bytes)?;
        let before_digest = before.as_ref().map(|bytes| digest(bytes));
        if before_digest != verified.before_index_digest {
            return Err(error(
                "memory_conflict",
                "Memory index changed after proposal verification",
            ));
        }
        if let Some(before) = &before {
            let _ = self.parse_document(
                std::str::from_utf8(before)
                    .map_err(|_| error("memory_format", "Memory index must be UTF-8"))?,
            )?;
        }
        check_revision(
            Some(&proposal.expected_revision),
            &self.snapshot()?.revision,
        )?;
        verify_root(&root, self)?;
        let generation = ledger
            .generation
            .checked_add(1)
            .ok_or_else(|| error("memory_limit", "Memory generation is exhausted"))?;
        directory(&root, "memory", true)?;
        directory(&root, "memory/.saya-history", true)?;
        clean_unjournaled(&root, &ledger, cancel)?;
        if let (Some(bytes), Some(version)) = (&before, &before_digest) {
            let path = history_path(version)?;
            if let Some(existing) = root.read(&path, self.limits.max_index_bytes)? {
                if digest(&existing) != *version {
                    return Err(error("memory_state", "Memory history digest is invalid"));
                }
            } else {
                atomic_write(&root, &path, bytes)?;
            }
            ledger.history.retain(|old| old.version != *version);
            ledger.history.push(MemoryHistoryVersion {
                version: version.clone(),
                generation,
                bytes: bytes.len(),
            });
        }
        let retired = if ledger.history.len() > self.limits.max_history_versions {
            ledger
                .history
                .drain(..ledger.history.len() - self.limits.max_history_versions)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        ledger.generation = generation;
        ledger.tombstones = verified.document.tombstones.clone();
        ledger.retractions = verified.document.retractions.clone();
        let receipt = MemoryOperationReceipt {
            operation_id: proposal.operation_id.clone(),
            change_digest: proposal.change_digest.clone(),
            generation,
            before_revision: proposal.expected_revision.clone(),
            document_digest: digest(rendered.as_bytes()),
            host_request_digest: proposal.host_request_digest.clone(),
        };
        ledger.operations.push(receipt.clone());
        ledger.validate(self.limits, &self.secrets.snapshot()?)?;
        let pending = Journal {
            schema_version: 1,
            before_index_digest: before_digest,
            after_index_digest: receipt.document_digest.clone(),
            ledger: ledger.clone(),
            receipt: receipt.clone(),
        };
        let journal_bytes = encode(&pending, state::max_state_bytes(self.limits))?;
        cancelled(cancel)?;
        atomic_write(&root, STAGE_PATH, rendered.as_bytes())?;
        #[cfg(test)]
        checkpoint(FaultPoint::AfterStage)?;
        cancelled(cancel)?;
        // Detect external editing after review and immediately before WAL.
        check_revision(
            Some(&proposal.expected_revision),
            &self.snapshot()?.revision,
        )?;
        verify_root(&root, self)?;
        let journal_write = atomic_write(&root, JOURNAL_PATH, &journal_bytes);
        if let Err(failure) = journal_write {
            // A rename can succeed before directory fsync fails. The journal's
            // presence determines whether the result has entered recovery.
            if root
                .read(JOURNAL_PATH, state::max_state_bytes(self.limits))?
                .is_none()
            {
                return Err(failure);
            }
            return Ok(MemoryCommitOutcome {
                status: MemoryCommitStatus::Unknown,
                receipt,
                revision: None,
                error: Some(failure.0),
                historical_copies_retained: !ledger.history.is_empty(),
                projection_refresh_required: true,
            });
        }
        if cancel.load(Ordering::Acquire) {
            return Ok(MemoryCommitOutcome {
                status: MemoryCommitStatus::Unknown,
                receipt,
                revision: None,
                error: Some(
                    error(
                        "cancelled",
                        "Memory operation requires recovery after cancellation",
                    )
                    .0,
                ),
                historical_copies_retained: !ledger.history.is_empty(),
                projection_refresh_required: true,
            });
        }
        #[cfg(test)]
        if let Err(failure) = checkpoint(FaultPoint::AfterJournal) {
            return Ok(MemoryCommitOutcome {
                status: MemoryCommitStatus::Unknown,
                receipt,
                revision: None,
                error: Some(failure.0),
                historical_copies_retained: !ledger.history.is_empty(),
                projection_refresh_required: true,
            });
        }
        if let Err(failure) = recover_locked(&root, self) {
            return Ok(MemoryCommitOutcome {
                status: MemoryCommitStatus::Unknown,
                receipt,
                revision: None,
                error: Some(failure.0),
                historical_copies_retained: !ledger.history.is_empty(),
                projection_refresh_required: true,
            });
        }
        let mut outcome = self.outcome(MemoryCommitStatus::Committed, receipt, &ledger);
        for version in retired {
            if let Err(failure) = remove(&root, &history_path(&version.version)?) {
                outcome.error = Some(failure.0);
            }
        }
        Ok(outcome)
    }
    fn outcome(
        &self,
        status: MemoryCommitStatus,
        receipt: MemoryOperationReceipt,
        ledger: &Ledger,
    ) -> MemoryCommitOutcome {
        let projection = self.snapshot();
        let (revision, error) = match projection {
            Ok(snapshot) => (Some(snapshot.revision), None),
            Err(failure) => (None, Some(failure.0)),
        };
        MemoryCommitOutcome {
            status,
            receipt,
            revision,
            error,
            historical_copies_retained: !ledger.history.is_empty(),
            projection_refresh_required: true,
        }
    }
    /// Recovery belongs to the host's already-authorized operation lifecycle.
    /// Read-only snapshot calls never invoke it or initialize a missing root.
    pub fn recover(&self) -> Result<Option<MemoryOperationReceipt>, BoundaryError> {
        self.recover_cancellable(&AtomicBool::new(false))
    }
    pub fn recover_cancellable(
        &self,
        cancel: &AtomicBool,
    ) -> Result<Option<MemoryOperationReceipt>, BoundaryError> {
        cancelled(cancel)?;
        let Some(root) = disk::open_root(&self.root)? else {
            return Ok(None);
        };
        // Avoid creating even a lock when no recovery record exists.
        if root
            .read(JOURNAL_PATH, state::max_state_bytes(self.limits))?
            .is_none()
        {
            return Ok(None);
        }
        let _lock = acquire(&root, cancel)?;
        recover_locked(&root, self)
    }
    pub fn history(&self) -> Result<Vec<MemoryHistoryVersion>, BoundaryError> {
        let _ = self.snapshot()?;
        let root = disk::open_root(&self.root)?;
        Ok(
            state::load(root.as_ref(), self.limits, &self.secrets.snapshot()?)?
                .0
                .history,
        )
    }
    /// Host retry bookkeeping, before rebuilding a proposal whose original
    /// expected revision may already have committed.
    pub fn operation_receipt(
        &self,
        operation_id: &str,
    ) -> Result<Option<MemoryOperationReceipt>, BoundaryError> {
        if !valid_id(operation_id, "op_") {
            return Err(error("memory_args", "Memory operation identity is invalid"));
        }
        let _ = self.snapshot()?;
        let root = disk::open_root(&self.root)?;
        let (ledger, _) = state::load(root.as_ref(), self.limits, &self.secrets.snapshot()?)?;
        Ok(ledger
            .operations
            .into_iter()
            .find(|receipt| receipt.operation_id == operation_id))
    }
    pub(super) fn history_document(&self, version: &str) -> Result<MemoryDocument, BoundaryError> {
        if !valid_digest(version) {
            return Err(error("memory_args", "Memory history version is invalid"));
        }
        let root = disk::open_root(&self.root)?
            .ok_or_else(|| error("memory_not_found", "Memory history is empty"))?;
        let (ledger, _) = state::load(Some(&root), self.limits, &self.secrets.snapshot()?)?;
        if !ledger.history.iter().any(|item| item.version == version) {
            return Err(error(
                "memory_not_found",
                "Memory history version is not retained",
            ));
        }
        let raw = root
            .read(&history_path(version)?, self.limits.max_index_bytes)?
            .ok_or_else(|| error("memory_not_found", "Memory history object is missing"))?;
        if digest(&raw) != version {
            return Err(error(
                "memory_state",
                "Memory history object digest is invalid",
            ));
        }
        self.parse_document(
            std::str::from_utf8(&raw)
                .map_err(|_| error("memory_format", "Memory history object must be UTF-8"))?,
        )
    }
}
