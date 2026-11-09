use super::workspace::{io_error, open_directory, options, sync};
use super::*;
use cap_std::fs::{Dir, DirBuilder, DirBuilderExt, MetadataExt as _, OpenOptionsExt};
use std::{
    fs::{self, File},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt as _, MetadataExt, OpenOptionsExt as _},
    },
    time::{Duration, Instant},
};

const MAX_BLOBS: usize = 16_384;
pub(super) struct Disk {
    root: Dir,
    path: PathBuf,
    identity: (u64, u64),
    limits: CheckpointLimits,
    #[cfg(test)]
    pub(super) fail_journal: std::sync::atomic::AtomicBool,
}
impl Disk {
    pub(super) fn open(path: &Path, limits: CheckpointLimits) -> Result<Self, BoundaryError> {
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(failure) if failure.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => {
                return Err(error(
                    "checkpoint_io",
                    "Private checkpoint root cannot be created",
                ));
            }
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)
            .map_err(io_error)?;
        let metadata = private_directory(&file)?;
        let canonical = fs::canonicalize(path).map_err(io_error)?;
        let named = fs::symlink_metadata(&canonical).map_err(io_error)?;
        if metadata.dev() != named.dev() || metadata.ino() != named.ino() {
            return Err(error(
                "checkpoint_permissions",
                "Checkpoint root changed while opening",
            ));
        }
        let disk = Self {
            root: Dir::from_std_file(file),
            path: canonical,
            identity: (metadata.dev(), metadata.ino()),
            limits,
            #[cfg(test)]
            fail_journal: std::sync::atomic::AtomicBool::new(false),
        };
        let _lock = disk.lock()?;
        for name in ["blobs", "snapshots", "operations"] {
            let mut builder = DirBuilder::new();
            builder.mode(0o700);
            match disk.root.create_dir_with(name, &builder) {
                Ok(()) => {}
                Err(failure) if failure.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => {
                    return Err(error(
                        "checkpoint_io",
                        "Private checkpoint directory cannot be created",
                    ));
                }
            }
            disk.directory(name)?;
        }
        sync(&disk.root)?;
        Ok(disk)
    }
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
    pub(super) fn lock(&self) -> Result<File, BoundaryError> {
        self.check_identity()?;
        let mut settings = options(true);
        settings.write(true).create(true).mode(0o600);
        let file = self
            .root
            .open_with("index.lock", &settings)
            .map_err(io_error)?
            .into_std();
        private_file(&file)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            // SAFETY: flock operates on this owned regular-file descriptor.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                break;
            }
            let failure = std::io::Error::last_os_error();
            if !matches!(
                failure.kind(),
                std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
            ) {
                return Err(error("checkpoint_io", "Checkpoint lock failed"));
            }
            if Instant::now() >= deadline {
                return Err(error("checkpoint_busy", "Checkpoint lock timed out"));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.check_identity()?;
        let actual = private_file(&file)?;
        let named = self.root.symlink_metadata("index.lock").map_err(io_error)?;
        if actual.dev() != named.dev() || actual.ino() != named.ino() {
            return Err(error(
                "checkpoint_permissions",
                "Checkpoint lock identity changed",
            ));
        }
        self.cleanup_temporaries()?;
        Ok(file)
    }
    fn check_identity(&self) -> Result<(), BoundaryError> {
        let named = fs::symlink_metadata(&self.path).map_err(io_error)?;
        if !named.is_dir() || named.dev() != self.identity.0 || named.ino() != self.identity.1 {
            return Err(error(
                "checkpoint_permissions",
                "Private checkpoint root identity changed",
            ));
        }
        private_directory(&self.root.try_clone().map_err(io_error)?.into_std_file())?;
        Ok(())
    }
    fn directory(&self, name: &str) -> Result<Dir, BoundaryError> {
        let directory = open_directory(&self.root, name)?;
        private_directory(&directory.try_clone().map_err(io_error)?.into_std_file())?;
        Ok(directory)
    }
    pub(super) fn blob(
        &self,
        digest: &str,
        expected_bytes: u64,
        limit: usize,
    ) -> Result<Vec<u8>, BoundaryError> {
        if !valid_hash(digest) || expected_bytes > limit as u64 {
            return Err(error(
                "checkpoint_corrupt",
                "Checkpoint blob identity is invalid",
            ));
        }
        let directory = self.directory("blobs")?;
        let bytes = read(&directory, &format!("{digest}.bin"), limit)?;
        if bytes.len() as u64 != expected_bytes || hash(&bytes) != digest {
            return Err(error(
                "checkpoint_corrupt",
                "Checkpoint blob differs from its sealed identity",
            ));
        }
        Ok(bytes)
    }
    pub(super) fn save_blob(&self, digest: &str, bytes: &[u8]) -> Result<(), BoundaryError> {
        if !valid_hash(digest) || hash(bytes) != digest || bytes.len() > self.limits.max_file_bytes
        {
            return Err(error(
                "checkpoint_corrupt",
                "Checkpoint blob source identity is invalid",
            ));
        }
        let directory = self.directory("blobs")?;
        let name = format!("{digest}.bin");
        match directory.symlink_metadata(&name) {
            Ok(_) => {
                self.blob(digest, bytes.len() as u64, self.limits.max_file_bytes)?;
                Ok(())
            }
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
                atomic_write(&directory, &name, bytes, true)
            }
            Err(_) => Err(error(
                "checkpoint_io",
                "Checkpoint blob state cannot be inspected",
            )),
        }
    }
    pub(super) fn snapshot(&self, token: &CheckpointToken) -> Result<Snapshot, BoundaryError> {
        if !valid_id(&token.id) || !valid_hash(&token.sha256) {
            return Err(error("checkpoint_corrupt", "Checkpoint token is invalid"));
        }
        let bytes = read(
            &self.directory("snapshots")?,
            &format!("{}.json", token.id),
            MAX_METADATA_BYTES,
        )?;
        if hash(&bytes) != token.sha256 {
            return Err(error(
                "checkpoint_conflict",
                "Checkpoint metadata changed after selection",
            ));
        }
        let snapshot: Snapshot = decode(&bytes)?;
        if snapshot.id != token.id {
            return Err(error(
                "checkpoint_corrupt",
                "Checkpoint filename and identity differ",
            ));
        }
        Ok(snapshot)
    }
    pub(super) fn save_snapshot(&self, snapshot: &Snapshot) -> Result<(), BoundaryError> {
        if !valid_id(&snapshot.id) {
            return Err(error("checkpoint_corrupt", "Checkpoint ID is invalid"));
        }
        let directory = self.directory("snapshots")?;
        let name = format!("{}.json", snapshot.id);
        if directory.symlink_metadata(&name).is_ok() {
            return Err(error(
                "checkpoint_conflict",
                "Checkpoint identity is already published",
            ));
        }
        atomic_write(&directory, &name, &encode(snapshot)?, false)
    }
    pub(super) fn snapshots(&self, limit: usize) -> Result<Vec<Snapshot>, BoundaryError> {
        let directory = self.directory("snapshots")?;
        let mut result = Vec::new();
        for name in names(&directory, limit)? {
            let id = json_name(&name)?;
            let snapshot: Snapshot = decode(&read(&directory, &name, MAX_METADATA_BYTES)?)?;
            if snapshot.id != id || snapshot.schema_version != 1 || snapshot.files.len() > 512 {
                return Err(error(
                    "checkpoint_corrupt",
                    "Stored checkpoint identity is invalid",
                ));
            }
            result.push(snapshot);
        }
        Ok(result)
    }
    pub(super) fn operation(&self, id: &str) -> Result<Option<ChangedFiles>, BoundaryError> {
        if !valid_id(id) {
            return Err(error(
                "checkpoint_corrupt",
                "Restore operation ID is invalid",
            ));
        }
        let directory = self.directory("operations")?;
        let name = format!("{id}.json");
        match directory.symlink_metadata(&name) {
            Ok(_) => {
                let outcome: ChangedFiles = decode(&read(&directory, &name, MAX_METADATA_BYTES)?)?;
                if outcome.operation_id != id {
                    return Err(error(
                        "checkpoint_corrupt",
                        "Restore operation identity differs from its filename",
                    ));
                }
                Ok(Some(outcome))
            }
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(error(
                "checkpoint_io",
                "Restore operation cannot be inspected",
            )),
        }
    }
    pub(super) fn operations(&self, limit: usize) -> Result<Vec<ChangedFiles>, BoundaryError> {
        let directory = self.directory("operations")?;
        let mut result = Vec::new();
        for name in names(&directory, limit)? {
            result.push(
                self.operation(&json_name(&name)?)?
                    .ok_or_else(|| error("checkpoint_conflict", "Restore journal disappeared"))?,
            );
        }
        Ok(result)
    }
    pub(super) fn save_operation(&self, outcome: &ChangedFiles) -> Result<(), BoundaryError> {
        if !valid_id(&outcome.operation_id) {
            return Err(error(
                "checkpoint_corrupt",
                "Restore operation ID is invalid",
            ));
        }
        let directory = self.directory("operations")?;
        let name = format!("{}.json", outcome.operation_id);
        let existing = match directory.symlink_metadata(&name) {
            Ok(metadata) => Some(metadata.len()),
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => {
                return Err(error(
                    "checkpoint_io",
                    "Restore journal state is unavailable",
                ));
            }
        };
        if existing.is_none()
            && names(&directory, self.limits.max_checkpoints.saturating_mul(4))?.len()
                >= self.limits.max_checkpoints.saturating_mul(4)
        {
            return Err(error(
                "checkpoint_limit",
                "Restore journal count exceeds its limit",
            ));
        }
        let required = self
            .usage()?
            .saturating_sub(existing.unwrap_or_default())
            .checked_add(MAX_METADATA_BYTES as u64)
            .ok_or_else(|| error("checkpoint_limit", "Checkpoint storage size overflows"))?;
        if required > self.limits.max_store_bytes {
            return Err(error(
                "checkpoint_limit",
                "Checkpoint storage cannot reserve a restore journal",
            ));
        }
        #[cfg(test)]
        if self.fail_journal.swap(false, Ordering::SeqCst) {
            return Err(error(
                "checkpoint_io",
                "Injected restore journal publication failure",
            ));
        }
        atomic_write(&directory, &name, &encode(outcome)?, false)
    }
    pub(super) fn check_capacity(
        &self,
        snapshot: &Snapshot,
        contents: &BTreeMap<String, Vec<u8>>,
        limits: CheckpointLimits,
    ) -> Result<(), BoundaryError> {
        if names(&self.directory("snapshots")?, limits.max_checkpoints)?.len()
            >= limits.max_checkpoints
        {
            return Err(error(
                "checkpoint_limit",
                "Checkpoint count exceeds its limit",
            ));
        }
        let directory = self.directory("blobs")?;
        let existing: BTreeSet<_> = names(&directory, MAX_BLOBS)?.into_iter().collect();
        let mut planned = BTreeMap::new();
        for entry in &snapshot.files {
            if let FileState::File { sha256, .. } = &entry.state
                && !existing.contains(&format!("{sha256}.bin"))
            {
                planned.insert(
                    sha256.clone(),
                    contents
                        .get(&entry.path)
                        .ok_or_else(|| {
                            error("checkpoint_corrupt", "Checkpoint source bytes are missing")
                        })?
                        .len(),
                );
            }
        }
        if existing.len() + planned.len() > MAX_BLOBS {
            return Err(error(
                "checkpoint_limit",
                "Checkpoint blob count exceeds its limit",
            ));
        }
        let required = planned
            .values()
            .try_fold(self.usage()?, |sum, size| sum.checked_add(*size as u64))
            .and_then(|total| total.checked_add(serde_json::to_vec(snapshot).ok()?.len() as u64))
            .ok_or_else(|| error("checkpoint_limit", "Checkpoint storage size overflows"))?;
        if required > limits.max_store_bytes {
            return Err(error(
                "checkpoint_limit",
                "Checkpoint storage exceeds its byte limit",
            ));
        }
        Ok(())
    }
    fn usage(&self) -> Result<u64, BoundaryError> {
        let mut total = 0u64;
        for (name, bound) in [
            ("blobs", MAX_BLOBS),
            ("snapshots", self.limits.max_checkpoints),
            ("operations", self.limits.max_checkpoints.saturating_mul(4)),
        ] {
            let directory = self.directory(name)?;
            for file in names(&directory, bound)? {
                if name == "blobs" {
                    if file
                        .strip_suffix(".bin")
                        .is_none_or(|digest| !valid_hash(digest))
                    {
                        return Err(error(
                            "checkpoint_corrupt",
                            "Checkpoint blob filename is invalid",
                        ));
                    }
                } else {
                    json_name(&file)?;
                }
                let file = directory
                    .open_with(file, &options(true))
                    .map_err(io_error)?
                    .into_std();
                let metadata = private_file(&file)?;
                if metadata.len() > self.limits.max_file_bytes.max(MAX_METADATA_BYTES) as u64 {
                    return Err(error(
                        "checkpoint_limit",
                        "Checkpoint object exceeds its size limit",
                    ));
                }
                total = total.checked_add(metadata.len()).ok_or_else(|| {
                    error("checkpoint_limit", "Checkpoint storage sizes overflow")
                })?;
            }
        }
        Ok(total)
    }
    fn cleanup_temporaries(&self) -> Result<(), BoundaryError> {
        for (name, limit) in [
            ("blobs", MAX_BLOBS),
            ("snapshots", self.limits.max_checkpoints),
            ("operations", self.limits.max_checkpoints.saturating_mul(4)),
        ] {
            match self.root.symlink_metadata(name) {
                Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    return Err(error(
                        "checkpoint_io",
                        "Checkpoint child directory cannot be inspected",
                    ));
                }
                Ok(_) => {}
            }
            let directory = self.directory(name)?;
            let mut changed = false;
            for entry in names(&directory, limit.saturating_add(16))? {
                if entry.strip_prefix(".tmp-").is_some_and(valid_id) {
                    let file = directory
                        .open_with(&entry, &options(true))
                        .map_err(io_error)?
                        .into_std();
                    private_file(&file)?;
                    directory.remove_file(&entry).map_err(io_error)?;
                    changed = true;
                }
            }
            if changed {
                sync(&directory)?;
            }
        }
        Ok(())
    }
    pub(super) fn collect_unreferenced_blobs(&self) -> Result<(), BoundaryError> {
        let mut retained = BTreeSet::new();
        for snapshot in self.snapshots(self.limits.max_checkpoints)? {
            for entry in snapshot.files {
                validate_state(&entry.state, self.limits)?;
                if let FileState::File { sha256, .. } = entry.state {
                    retained.insert(format!("{sha256}.bin"));
                }
            }
        }
        let directory = self.directory("blobs")?;
        let mut changed = false;
        for name in names(&directory, MAX_BLOBS)? {
            if name
                .strip_suffix(".bin")
                .is_none_or(|digest| !valid_hash(digest))
            {
                return Err(error(
                    "checkpoint_corrupt",
                    "Checkpoint blob filename is invalid",
                ));
            }
            if !retained.contains(&name) {
                let file = directory
                    .open_with(&name, &options(true))
                    .map_err(io_error)?
                    .into_std();
                private_file(&file)?;
                directory.remove_file(&name).map_err(io_error)?;
                changed = true;
            }
        }
        if changed {
            sync(&directory)?;
        }
        Ok(())
    }
}
fn private_directory(file: &File) -> Result<fs::Metadata, BoundaryError> {
    let metadata = file.metadata().map_err(io_error)?;
    // SAFETY: geteuid has no pointer arguments or ownership requirements.
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(error(
            "checkpoint_permissions",
            "Checkpoint directory must be private and owned",
        ));
    }
    Ok(metadata)
}
fn private_file(file: &File) -> Result<fs::Metadata, BoundaryError> {
    let metadata = file.metadata().map_err(io_error)?;
    // SAFETY: geteuid has no pointer arguments or ownership requirements.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(error(
            "checkpoint_permissions",
            "Checkpoint metadata must be private owned regular files without hard links",
        ));
    }
    Ok(metadata)
}
fn names(directory: &Dir, limit: usize) -> Result<Vec<String>, BoundaryError> {
    let mut names = Vec::new();
    for entry in directory.entries().map_err(io_error)? {
        let name = entry
            .map_err(io_error)?
            .file_name()
            .into_string()
            .map_err(|_| {
                error(
                    "checkpoint_corrupt",
                    "Checkpoint object filename is not UTF-8",
                )
            })?;
        names.push(name);
        if names.len() > limit {
            return Err(error(
                "checkpoint_limit",
                "Checkpoint directory entry count exceeds its limit",
            ));
        }
    }
    names.sort();
    Ok(names)
}
fn read(directory: &Dir, name: &str, limit: usize) -> Result<Vec<u8>, BoundaryError> {
    let file = directory
        .open_with(name, &options(true))
        .map_err(io_error)?
        .into_std();
    private_file(&file)?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > limit {
        return Err(error(
            "checkpoint_limit",
            "Checkpoint object exceeds its read limit",
        ));
    }
    Ok(bytes)
}
fn encode(value: &impl Serialize) -> Result<Vec<u8>, BoundaryError> {
    let bytes = serde_json::to_vec(value).map_err(|_| {
        error(
            "checkpoint_corrupt",
            "Checkpoint metadata cannot be encoded",
        )
    })?;
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(error(
            "checkpoint_limit",
            "Checkpoint metadata exceeds its byte limit",
        ));
    }
    Ok(bytes)
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, BoundaryError> {
    serde_json::from_slice(bytes).map_err(|_| {
        error(
            "checkpoint_corrupt",
            "Checkpoint metadata cannot be decoded",
        )
    })
}
fn json_name(name: &str) -> Result<String, BoundaryError> {
    name.strip_suffix(".json")
        .filter(|id| valid_id(id))
        .map(str::to_owned)
        .ok_or_else(|| {
            error(
                "checkpoint_corrupt",
                "Checkpoint metadata filename is invalid",
            )
        })
}
fn atomic_write(
    directory: &Dir,
    name: &str,
    bytes: &[u8],
    readonly: bool,
) -> Result<(), BoundaryError> {
    let temporary = format!(".tmp-{}", Uuid::now_v7());
    let mut settings = options(false);
    settings.write(true).create_new(true).mode(0o600);
    let mut file = directory
        .open_with(&temporary, &settings)
        .map_err(io_error)?
        .into_std();
    let result = (|| {
        file.write_all(bytes).map_err(io_error)?;
        if readonly {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o400))
                .map_err(io_error)?;
        }
        file.sync_all().map_err(io_error)?;
        directory
            .rename(&temporary, directory, name)
            .map_err(io_error)?;
        sync(directory).map_err(|_| {
            error(
                "checkpoint_commit_unknown",
                "Checkpoint metadata publication durability is unknown",
            )
        })
    })();
    if result.is_err() {
        let _ = directory.remove_file(&temporary);
    }
    result
}
