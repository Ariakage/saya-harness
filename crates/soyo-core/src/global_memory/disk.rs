use super::*;
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
use std::{
    fs::File,
    io::Read,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt as _},
    },
};

pub(super) struct MemoryRoot {
    pub(super) dir: Dir,
}

pub(super) fn workspace_identity(path: &Path) -> Result<WorkspaceIdentity, BoundaryError> {
    let canonical = std::fs::canonicalize(path)
        .map_err(|_| error("memory_scope", "Workspace cannot be resolved"))?;
    let metadata = std::fs::metadata(&canonical)
        .map_err(|_| error("memory_scope", "Workspace identity is unavailable"))?;
    if !metadata.is_dir() {
        return Err(error("memory_scope", "Workspace must be a directory"));
    }
    let mut hash = Sha256::new();
    hash.update(b"saya-workspace-v1\0");
    hash.update(canonical.as_os_str().as_bytes());
    hash.update(metadata.dev().to_be_bytes());
    hash.update(metadata.ino().to_be_bytes());
    let hash = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(WorkspaceIdentity {
        id: format!("ws_{hash}"),
    })
}

pub(super) fn validate_resource_path(path: &str) -> Result<(), BoundaryError> {
    if path.len() > 512
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || !path.starts_with("memory/")
        || !path.ends_with(".md")
    {
        return Err(error("memory_path", "Memory topic path is invalid"));
    }
    let components = path.split('/').collect::<Vec<_>>();
    if components.len() > 10
        || components
            .iter()
            .any(|part| part.is_empty() || part.starts_with('.') || part.len() > 128)
    {
        return Err(error("memory_path", "Memory topic path is invalid"));
    }
    Ok(())
}

fn owned(metadata: &std::fs::Metadata, directory: bool) -> bool {
    // SAFETY: geteuid has no pointer arguments or ownership requirements.
    metadata.uid() == unsafe { libc::geteuid() }
        && metadata.mode() & 0o022 == 0
        && if directory {
            metadata.is_dir()
        } else {
            metadata.is_file() && metadata.nlink() == 1
        }
}

pub(super) fn open_root(path: &Path) -> Result<Option<MemoryRoot>, BoundaryError> {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(error("memory_path", "Memory root is unavailable or linked")),
    };
    let metadata = file
        .metadata()
        .map_err(|_| error("memory_io", "Memory root metadata is unavailable"))?;
    if !owned(&metadata, true) {
        return Err(error(
            "memory_permissions",
            "Memory root must be owned and protected from external writes",
        ));
    }
    Ok(Some(MemoryRoot {
        dir: Dir::from_std_file(file),
    }))
}

impl MemoryRoot {
    pub(super) fn file(&self, path: &str) -> Result<Option<File>, BoundaryError> {
        let mut components = path.split('/').collect::<Vec<_>>();
        let name = components
            .pop()
            .ok_or_else(|| error("memory_path", "Memory filename is invalid"))?;
        let mut parent = self
            .dir
            .try_clone()
            .map_err(|_| error("memory_io", "Memory directory cannot be cloned"))?;
        for component in components {
            let mut options = OpenOptions::new();
            options
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
            let file = match parent.open_with(component, &options) {
                Ok(file) => file.into_std(),
                Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(_) => {
                    return Err(error(
                        "memory_path",
                        "Memory topic directory is unavailable or linked",
                    ));
                }
            };
            let metadata = file
                .metadata()
                .map_err(|_| error("memory_io", "Memory directory metadata is unavailable"))?;
            if !owned(&metadata, true) {
                return Err(error(
                    "memory_permissions",
                    "Memory topic directory is not protected",
                ));
            }
            if component.starts_with(".saya-") && metadata.mode() & 0o077 != 0 {
                return Err(error(
                    "memory_permissions",
                    "Memory internal directory must be private",
                ));
            }
            parent = Dir::from_std_file(file);
        }
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        let file = match parent.open_with(name, &options) {
            Ok(file) => file.into_std(),
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(error("memory_path", "Memory file is unavailable or linked")),
        };
        let metadata = file
            .metadata()
            .map_err(|_| error("memory_io", "Memory file metadata is unavailable"))?;
        if !owned(&metadata, false) {
            return Err(error(
                "memory_permissions",
                "Memory file must be regular, owned and unlinked",
            ));
        }
        if path.starts_with("memory/.saya-") && metadata.mode() & 0o077 != 0 {
            return Err(error(
                "memory_permissions",
                "Memory internal file must be private",
            ));
        }
        Ok(Some(file))
    }

    pub(super) fn read(&self, path: &str, max: usize) -> Result<Option<Vec<u8>>, BoundaryError> {
        self.read_cancellable(path, max, &AtomicBool::new(false))
    }
    pub(super) fn read_cancellable(
        &self,
        path: &str,
        max: usize,
        cancel: &AtomicBool,
    ) -> Result<Option<Vec<u8>>, BoundaryError> {
        check_cancelled(cancel)?;
        let Some(mut file) = self.file(path)? else {
            return Ok(None);
        };
        let before = file
            .metadata()
            .map_err(|_| error("memory_io", "Memory file metadata is unavailable"))?;
        if before.len() > max as u64 {
            return Err(error("memory_limit", "Memory file exceeds its byte limit"));
        }
        let mut bytes = Vec::with_capacity(before.len() as usize);
        let mut buffer = [0u8; 8192];
        loop {
            check_cancelled(cancel)?;
            let count = file
                .read(&mut buffer)
                .map_err(|_| error("memory_io", "Memory file cannot be read"))?;
            if count == 0 {
                break;
            }
            if bytes.len().saturating_add(count) > max {
                return Err(error("memory_limit", "Memory file exceeds its byte limit"));
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        check_cancelled(cancel)?;
        let after = file
            .metadata()
            .map_err(|_| error("memory_io", "Memory file metadata is unavailable"))?;
        let named = self
            .file(path)?
            .ok_or_else(|| error("memory_conflict", "Memory file changed while reading"))?
            .metadata()
            .map_err(|_| error("memory_io", "Memory file metadata is unavailable"))?;
        if before.dev() != named.dev()
            || before.ino() != named.ino()
            || before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
            || bytes.len() as u64 != after.len()
        {
            return Err(error(
                "memory_conflict",
                "Memory file changed while reading",
            ));
        }
        Ok(Some(bytes))
    }
}

impl GlobalMemoryStore {
    pub(super) fn load(&self) -> Result<Loaded, BoundaryError> {
        self.load_cancellable(&AtomicBool::new(false))
    }
    pub(super) fn load_cancellable(&self, cancel: &AtomicBool) -> Result<Loaded, BoundaryError> {
        check_cancelled(cancel)?;
        let secrets = self.secrets.snapshot()?;
        let root = open_root(&self.root)?;
        // Propagate malformed paths and I/O errors rather than treating an
        // unreadable recovery record as absent.
        if let Some(root) = &root
            && root
                .read_cancellable(
                    state::JOURNAL_PATH,
                    state::max_state_bytes(self.limits),
                    cancel,
                )?
                .is_some()
        {
            return Err(error(
                "memory_recovery_required",
                "Memory has an unfinished durable operation",
            ));
        }
        let (ledger, ledger_raw) = state::load(root.as_ref(), self.limits, &secrets)?;
        check_cancelled(cancel)?;
        let raw = match &root {
            Some(root) => {
                root.read_cancellable("MEMORY.md", self.limits.max_index_bytes, cancel)?
            }
            None => None,
        };
        let mut document = match &raw {
            Some(raw) => document::parse(
                std::str::from_utf8(raw)
                    .map_err(|_| error("memory_format", "Memory index must be UTF-8"))?,
                self.limits,
                &secrets,
            )?,
            None => MemoryDocument::default(),
        };
        state::merge(&mut document, &ledger)?;
        document::validate(&document, self.limits, &secrets)?;
        let active = document::active_document(&document);
        let entries = active
            .entries
            .iter()
            .filter(|entry| entry.scope.visible_in(&self.workspace))
            .cloned()
            .collect::<Vec<_>>();
        let mut paths = std::collections::BTreeSet::new();
        let mut source_ids = std::collections::BTreeSet::new();
        for entry in &entries {
            paths.extend(entry.resources.iter().cloned());
            for fact in &entry.facts {
                source_ids.extend(fact.sources.iter().map(|source| source.source_id.clone()));
            }
        }
        let mut resources = BTreeMap::new();
        let mut total_bytes = 0usize;
        for path in paths {
            check_cancelled(cancel)?;
            let bytes = root
                .as_ref()
                .ok_or_else(|| error("memory_not_found", "Memory root is unavailable"))?
                .read_cancellable(&path, self.limits.max_resource_bytes, cancel)?
                .ok_or_else(|| error("memory_not_found", "Referenced memory topic is missing"))?;
            total_bytes = total_bytes
                .checked_add(bytes.len())
                .ok_or_else(|| error("memory_limit", "Memory resource sizes overflow"))?;
            if total_bytes > self.limits.max_total_resource_bytes {
                return Err(error("memory_limit", "Memory resource budget exceeded"));
            }
            let text = String::from_utf8(bytes)
                .map_err(|_| error("memory_format", "Memory topic must be UTF-8"))?;
            if crate::redaction::contains_secret(&text, &secrets) {
                return Err(error(
                    "sensitive_data",
                    "Memory topic contains protected data",
                ));
            }
            resources.insert(
                path.clone(),
                MemoryResource {
                    info: MemoryResourceInfo {
                        path,
                        revision: digest(text.as_bytes()),
                        bytes: text.len(),
                    },
                    text,
                },
            );
        }
        // Re-read all selected objects so a concurrent editor cannot silently
        // assemble a projection from changing source files.
        if let Some(root) = &root {
            if root.read_cancellable("MEMORY.md", self.limits.max_index_bytes, cancel)? != raw {
                return Err(error(
                    "memory_conflict",
                    "Memory index changed while loading topics",
                ));
            }
            for resource in resources.values() {
                let current = root.read_cancellable(
                    &resource.info.path,
                    self.limits.max_resource_bytes,
                    cancel,
                )?;
                if current.as_deref() != Some(resource.text.as_bytes()) {
                    return Err(error(
                        "memory_conflict",
                        "Memory topic changed while loading",
                    ));
                }
            }
            if root.read_cancellable(
                state::STATE_PATH,
                state::max_state_bytes(self.limits),
                cancel,
            )? != ledger_raw
                || root
                    .read_cancellable(
                        state::JOURNAL_PATH,
                        state::max_state_bytes(self.limits),
                        cancel,
                    )?
                    .is_some()
            {
                return Err(error(
                    "memory_conflict",
                    "Memory commit state changed while loading",
                ));
            }
        }
        let mut hash = Sha256::new();
        hash.update(b"saya-memory-snapshot-v1\0");
        hash.update(self.workspace.id.as_bytes());
        hash.update([u8::from(raw.is_some())]);
        if let Some(raw) = &raw {
            hash.update(raw);
        }
        if let Some(ledger_raw) = &ledger_raw {
            hash.update(ledger_raw);
        }
        for resource in resources.values() {
            hash.update(resource.info.path.as_bytes());
            hash.update(resource.info.revision.as_bytes());
        }
        let forget = serde_json::to_vec(&(
            document
                .tombstones
                .iter()
                .filter(|stone| stone.scope.visible_in(&self.workspace))
                .collect::<Vec<_>>(),
            &document.retractions,
        ))
        .map_err(|_| error("memory_format", "Memory forget state cannot be encoded"))?;
        let revision = hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let snapshot = MemorySnapshot {
            exists: raw.is_some(),
            revision,
            workspace: self.workspace.clone(),
            entries,
            sources: active
                .sources
                .iter()
                .filter(|source| source_ids.contains(&source.id))
                .cloned()
                .collect(),
            resources: resources
                .values()
                .map(|resource| resource.info.clone())
                .collect(),
            forget_revision: digest(&forget),
        };
        check_cancelled(cancel)?;
        Ok(Loaded {
            document,
            snapshot,
            resources,
            index_digest: raw.as_ref().map(|bytes| digest(bytes)),
        })
    }
}
