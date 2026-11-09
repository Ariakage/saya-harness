use super::*;
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
use std::{
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt as _, PermissionsExt},
};
use unicode_normalization::UnicodeNormalization;

pub(super) struct Workspace {
    root: Dir,
    pub identity: WorkspaceIdentity,
    pub kind: WorkspaceKind,
    protected: Vec<String>,
}
pub(super) struct FileRead {
    pub version: FileVersion,
    pub bytes: Vec<u8>,
}
#[derive(Serialize, PartialEq, Eq)]
struct NodeIdentity {
    device: u64,
    inode: u64,
    bytes: u64,
    mode: u32,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl NodeIdentity {
    fn from(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            bytes: metadata.len(),
            mode: metadata.mode(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}
impl Workspace {
    pub(super) fn open(path: &Path, protected: &[PathBuf]) -> Result<Self, BoundaryError> {
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| error("checkpoint_path", "Workspace cannot be opened safely"))?;
        let metadata = file.metadata().map_err(io_error)?;
        let canonical = fs::canonicalize(path).map_err(io_error)?;
        let named = fs::symlink_metadata(&canonical).map_err(io_error)?;
        if !metadata.is_dir()
            || named.dev() != metadata.dev()
            || named.ino() != metadata.ino()
            || canonical.to_str().is_none_or(|path| {
                path.len() > 4096 || path.chars().any(crate::text::is_terminal_control)
            })
        {
            return Err(error(
                "checkpoint_conflict",
                "Workspace identity changed while opening",
            ));
        }
        let root = Dir::from_std_file(file);
        let kind = match root.symlink_metadata(".git") {
            Ok(metadata) if metadata.is_dir() => {
                open_directory(&root, ".git")?;
                WorkspaceKind::GitDirectory
            }
            Ok(metadata) if metadata.is_file() => {
                let file = root
                    .open_with(".git", &options(true))
                    .map_err(io_error)?
                    .into_std();
                regular_file(&file)?;
                let mut bytes = Vec::new();
                file.take(4097).read_to_end(&mut bytes).map_err(io_error)?;
                if bytes.len() > 4096 || !bytes.starts_with(b"gitdir: ") {
                    return Err(error(
                        "checkpoint_unsupported",
                        "Git worktree marker is unsupported",
                    ));
                }
                WorkspaceKind::GitWorktree
            }
            Ok(_) => {
                return Err(error(
                    "checkpoint_path",
                    "Git metadata marker is linked or unsupported",
                ));
            }
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
                WorkspaceKind::Directory
            }
            Err(_) => {
                return Err(error(
                    "checkpoint_io",
                    "Git workspace marker cannot be inspected",
                ));
            }
        };
        let mut protected_keys = Vec::new();
        for protected in protected {
            let absolute = if protected.is_absolute() {
                protected.clone()
            } else {
                canonical.join(protected)
            };
            let resolved = match fs::canonicalize(&absolute) {
                Ok(path) => path,
                Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => absolute,
                Err(_) => {
                    return Err(error(
                        "checkpoint_path",
                        "Protected checkpoint path cannot be resolved",
                    ));
                }
            };
            if resolved.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            }) {
                return Err(error(
                    "checkpoint_path",
                    "Protected checkpoint path is not normalized",
                ));
            }
            protected_keys.push(path_key(&resolved)?);
        }
        Ok(Self {
            root,
            identity: WorkspaceIdentity {
                path: canonical,
                device: metadata.dev(),
                inode: metadata.ino(),
            },
            kind,
            protected: protected_keys,
        })
    }
    pub(super) fn check_identity(&self) -> Result<(), BoundaryError> {
        let named = fs::symlink_metadata(&self.identity.path).map_err(io_error)?;
        if !named.is_dir()
            || named.dev() != self.identity.device
            || named.ino() != self.identity.inode
        {
            return Err(error(
                "checkpoint_conflict",
                "Workspace root identity changed",
            ));
        }
        Ok(())
    }
    pub(super) fn validate_path(&self, path: &str) -> Result<(), BoundaryError> {
        let components: Vec<_> = path.split('/').collect();
        if path.is_empty()
            || path.len() > 1024
            || components.len() > 16
            || path.contains('\\')
            || path.contains(':')
            || path.chars().any(crate::text::is_terminal_control)
            || components
                .iter()
                .any(|part| part.is_empty() || matches!(*part, "." | ".."))
        {
            return Err(error(
                "checkpoint_path",
                "Checkpoint path must be a normalized workspace-relative filename",
            ));
        }
        let folded: Vec<_> = components.iter().map(|part| fold(part)).collect();
        if folded.iter().any(|part| part == ".git")
            || folded
                .windows(2)
                .any(|parts| parts[0] == ".saya" && parts[1] == "local")
        {
            return Err(error(
                "checkpoint_path",
                "Checkpoint paths cannot address repository or harness control data",
            ));
        }
        let key = path_key(&self.identity.path.join(path))?;
        if self.protected.iter().any(|protected| {
            key == *protected
                || key
                    .strip_prefix(protected)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        }) {
            return Err(error(
                "checkpoint_path",
                "Checkpoint path addresses protected host storage",
            ));
        }
        Ok(())
    }
    fn parent(&self, path: &str) -> Result<(Dir, String), BoundaryError> {
        self.check_identity()?;
        self.validate_path(path)?;
        let parts: Vec<_> = path.split('/').collect();
        let (name, parents) = parts
            .split_last()
            .ok_or_else(|| error("checkpoint_path", "Checkpoint filename is missing"))?;
        let mut directory = self.root.try_clone().map_err(io_error)?;
        for component in parents {
            directory = open_directory(&directory, component)?;
        }
        Ok((directory, (*name).into()))
    }
    pub(super) fn read(
        &self,
        path: &str,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<FileRead, BoundaryError> {
        cancelled(cancel)?;
        let (parent, name) = self.parent(path)?;
        let parent_metadata = parent
            .try_clone()
            .map_err(io_error)?
            .into_std_file()
            .metadata()
            .map_err(io_error)?;
        let mut file = match parent.open_with(&name, &options(true)) {
            Ok(file) => file.into_std(),
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
                let state = FileState::Absent;
                let observation =
                    observation(&(&state, parent_metadata.dev(), parent_metadata.ino()))?;
                return Ok(FileRead {
                    version: FileVersion { state, observation },
                    bytes: Vec::new(),
                });
            }
            Err(_) => {
                return Err(error(
                    "checkpoint_path",
                    "Checkpoint file is linked, unavailable, or unsafe",
                ));
            }
        };
        let before = regular_file(&file)?;
        if before.len() > limit as u64 {
            return Err(error(
                "checkpoint_limit",
                "Checkpoint file exceeds its byte limit",
            ));
        }
        let mut bytes = Vec::new();
        let mut chunk = [0; 16 * 1024];
        loop {
            cancelled(cancel)?;
            let count = file.read(&mut chunk).map_err(io_error)?;
            if count == 0 {
                break;
            }
            if bytes.len().saturating_add(count) > limit {
                return Err(error(
                    "checkpoint_limit",
                    "Checkpoint file exceeds its byte limit",
                ));
            }
            bytes.extend_from_slice(&chunk[..count]);
        }
        let after = regular_file(&file)?;
        let named = parent
            .open_with(&name, &options(true))
            .map_err(io_error)?
            .into_std();
        let named = regular_file(&named)?;
        if NodeIdentity::from(&before) != NodeIdentity::from(&after)
            || NodeIdentity::from(&after) != NodeIdentity::from(&named)
            || after.len() != bytes.len() as u64
        {
            return Err(error(
                "checkpoint_conflict",
                "Checkpoint file changed during observation",
            ));
        }
        if bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
            return Err(error(
                "checkpoint_unsupported",
                "Opaque checkpoint files are unsupported",
            ));
        }
        self.check_identity()?;
        self.verify_parent(path, &parent)?;
        let state = FileState::File {
            sha256: hash(&bytes),
            bytes: bytes.len() as u64,
            mode: after.mode() & 0o777,
        };
        Ok(FileRead {
            version: FileVersion {
                observation: observation(&(&state, NodeIdentity::from(&after)))?,
                state,
            },
            bytes,
        })
    }

    pub(super) fn publish(
        &self,
        operation: &RestoreOperation,
        bytes: &[u8],
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<(), BoundaryError> {
        if bytes.len() > limit {
            return Err(error(
                "checkpoint_limit",
                "Restore bytes exceed their limit",
            ));
        }
        let current = self.read(&operation.path, limit, cancel)?;
        if current.version != operation.expected {
            return Err(error(
                "checkpoint_conflict",
                "File changed before restore publication",
            ));
        }
        let (parent, name) = self.parent(&operation.path)?;
        if operation.desired == FileState::Absent {
            self.verify_parent(&operation.path, &parent)?;
            if self.read(&operation.path, limit, cancel)?.version != operation.expected {
                return Err(error(
                    "checkpoint_conflict",
                    "File changed before restore deletion",
                ));
            }
            cancelled(cancel)?;
            parent.remove_file(&name).map_err(io_error)?;
            return sync(&parent);
        }
        let FileState::File {
            sha256,
            bytes: length,
            mode,
        } = &operation.desired
        else {
            return Err(error("checkpoint_corrupt", "Restore state is invalid"));
        };
        if hash(bytes) != *sha256 || bytes.len() as u64 != *length {
            return Err(error(
                "checkpoint_corrupt",
                "Restore bytes differ from their sealed identity",
            ));
        }
        let temporary = format!(".saya-checkpoint-{}.tmp", Uuid::now_v7());
        let mut settings = options(false);
        settings.write(true).create_new(true).mode(0o600);
        let mut file = parent
            .open_with(&temporary, &settings)
            .map_err(io_error)?
            .into_std();
        let result = (|| {
            for chunk in bytes.chunks(16 * 1024) {
                cancelled(cancel)?;
                file.write_all(chunk).map_err(io_error)?;
            }
            file.set_permissions(fs::Permissions::from_mode(*mode))
                .map_err(io_error)?;
            file.sync_all().map_err(io_error)?;
            self.verify_parent(&operation.path, &parent)?;
            if self.read(&operation.path, limit, cancel)?.version != operation.expected {
                return Err(error(
                    "checkpoint_conflict",
                    "File changed before restore replacement",
                ));
            }
            cancelled(cancel)?;
            if operation.expected.state == FileState::Absent {
                parent
                    .hard_link(&temporary, &parent, &name)
                    .map_err(|failure| {
                        if failure.kind() == std::io::ErrorKind::AlreadyExists {
                            error(
                                "checkpoint_conflict",
                                "A new restore file appeared before publication",
                            )
                        } else {
                            io_error(failure)
                        }
                    })?;
                parent.remove_file(&temporary).map_err(io_error)?;
            } else {
                parent
                    .rename(&temporary, &parent, &name)
                    .map_err(io_error)?;
            }
            sync(&parent)
        })();
        if result.is_err() {
            let _ = parent.remove_file(&temporary);
        }
        result
    }
    fn verify_parent(&self, path: &str, held: &Dir) -> Result<(), BoundaryError> {
        let (current, _) = self.parent(path)?;
        let a = current.into_std_file().metadata().map_err(io_error)?;
        let b = held
            .try_clone()
            .map_err(io_error)?
            .into_std_file()
            .metadata()
            .map_err(io_error)?;
        if a.dev() != b.dev() || a.ino() != b.ino() {
            return Err(error(
                "checkpoint_conflict",
                "Restore parent directory identity changed",
            ));
        }
        Ok(())
    }
}
fn observation(value: &impl Serialize) -> Result<String, BoundaryError> {
    Ok(hash(&serde_json::to_vec(value).map_err(|_| {
        error("checkpoint_corrupt", "File version cannot be encoded")
    })?))
}
fn fold(value: &str) -> String {
    let composed: String = value.nfc().collect();
    unicase::UniCase::unicode(composed)
        .to_folded_case()
        .nfc()
        .collect()
}
fn path_key(path: &Path) -> Result<String, BoundaryError> {
    path.to_str()
        .map(fold)
        .ok_or_else(|| error("checkpoint_path", "Checkpoint host path must be UTF-8"))
}
pub(super) fn options(read: bool) -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .read(read)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    options
}
pub(super) fn open_directory(parent: &Dir, name: &str) -> Result<Dir, BoundaryError> {
    let mut settings = options(true);
    settings
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    Ok(Dir::from_std_file(
        parent
            .open_with(name, &settings)
            .map_err(io_error)?
            .into_std(),
    ))
}
fn regular_file(file: &File) -> Result<fs::Metadata, BoundaryError> {
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.mode() & 0o7000 != 0 {
        return Err(error(
            "checkpoint_unsupported",
            "Checkpoint files must be ordinary unlinked files without special mode bits",
        ));
    }
    Ok(metadata)
}
pub(super) fn sync(directory: &Dir) -> Result<(), BoundaryError> {
    directory
        .try_clone()
        .map_err(io_error)?
        .into_std_file()
        .sync_all()
        .map_err(io_error)
}
pub(super) fn io_error(_: std::io::Error) -> BoundaryError {
    error("checkpoint_io", "Checkpoint filesystem operation failed")
}
