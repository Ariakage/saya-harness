//! Explicit file publication for rendered session exports.
use cap_std::fs::{Dir, DirBuilder, DirBuilderExt, MetadataExt as _, OpenOptions, OpenOptionsExt};
use saya_core::{
    boundary::Cancellation,
    contract::{BoundaryError, ErrorInfo, RunId, WorkMode},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

fn failure(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}
fn check_cancel(cancel: &dyn Cancellation) -> Result<(), BoundaryError> {
    if cancel.is_cancelled() {
        Err(failure("cancelled", "export cancelled"))
    } else {
        Ok(())
    }
}
/// Publish complete bytes with exclusive creation and private permissions.
/// The parent must exist. Cancellation is observed before the atomic commit.
/// A failure after commit reports export_publish_uncertain for explicit inspection.
pub fn publish(
    target: &Path,
    bytes: &[u8],
    workspace: &Path,
    mode: WorkMode,
    cancel: &dyn Cancellation,
) -> Result<Value, BoundaryError> {
    publish_with_roots(target, bytes, &[workspace], mode, cancel)
}

/// Publish a session export, protecting both current and saved workspaces in PLAN.
/// Relative targets are resolved by the caller before publication.
pub fn publish_session(
    target: &Path,
    bytes: &[u8],
    workspace: &Path,
    session_workspace: &Path,
    mode: WorkMode,
    cancel: &dyn Cancellation,
) -> Result<Value, BoundaryError> {
    publish_with_roots(target, bytes, &[workspace, session_workspace], mode, cancel)
}

/// A read-only captured destination. Ownership retains the approved directory
/// descriptor and binds both the selected alias and its canonical identity.
/// Capturing this value creates no files. It is intentionally single-use.
pub struct PreparedTarget {
    path: PathBuf,
    requested_path: PathBuf,
    parent: PathBuf,
    original_parent: PathBuf,
    name: OsString,
    dir: Dir,
    device: u64,
    inode: u64,
}
impl PreparedTarget {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn requested_path(&self) -> &Path {
        &self.requested_path
    }
    fn check_parent(&self) -> Result<(), BoundaryError> {
        let selected = std::fs::canonicalize(&self.original_parent)
            .map_err(|_| failure("export_path_changed", "selected export directory changed"))?;
        let canonical = std::fs::canonicalize(&self.parent)
            .map_err(|_| failure("export_path_changed", "export directory changed"))?;
        let named = std::fs::symlink_metadata(&self.parent)
            .map_err(|_| failure("export_path_changed", "export directory changed"))?;
        let held = self.dir.dir_metadata().map_err(|_| {
            failure(
                "export_path_changed",
                "approved export directory is unavailable",
            )
        })?;
        if selected != self.parent
            || canonical != self.parent
            || !named.is_dir()
            || named.dev() != self.device
            || named.ino() != self.inode
            || held.dev() != self.device
            || held.ino() != self.inode
        {
            return Err(failure(
                "export_path_changed",
                "export directory differs from its captured identity",
            ));
        }
        Ok(())
    }
}
/// Resolve and hold the actual publication parent before human review.
/// The final name remains exclusive-create at publication time.
pub fn prepare_target(target: &Path) -> Result<PreparedTarget, BoundaryError> {
    let name = target
        .file_name()
        .ok_or_else(|| failure("invalid_export_path", "export requires a file name"))?
        .to_owned();
    let requested_path = if target.is_absolute() {
        target.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|_| failure("invalid_export_path", "current directory is unavailable"))?
            .join(target)
    };
    let original_parent = requested_path
        .parent()
        .ok_or_else(|| failure("invalid_export_path", "export requires a parent directory"))?
        .to_owned();
    let parent = std::fs::canonicalize(&original_parent).map_err(|_| {
        failure(
            "invalid_export_path",
            "export parent directory is unavailable",
        )
    })?;
    let path = parent.join(&name);
    if path.to_str().is_none() {
        return Err(failure("invalid_export_path", "export path must be UTF-8"));
    }
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(&parent)
        .map_err(|_| failure("export_io", "cannot open export directory"))?;
    let metadata = file
        .metadata()
        .map_err(|_| failure("export_io", "cannot inspect export directory"))?;
    let captured = PreparedTarget {
        path,
        requested_path,
        parent,
        original_parent,
        name,
        device: metadata.dev(),
        inode: metadata.ino(),
        dir: Dir::from_std_file(file),
    };
    captured.check_parent()?;
    Ok(captured)
}
/// Publish only through the directory descriptor captured before review.
/// Named-path changes reject before publication or report an uncertain result
/// after publication. Existing destinations are never replaced.
pub fn publish_prepared(
    target: PreparedTarget,
    bytes: &[u8],
    workspace: &Path,
    mode: WorkMode,
    cancel: &dyn Cancellation,
) -> Result<Value, BoundaryError> {
    publish_prepared_with_roots(target, bytes, &[workspace], mode, cancel)
}
fn publish_with_roots(
    target: &Path,
    bytes: &[u8],
    workspaces: &[&Path],
    mode: WorkMode,
    cancel: &dyn Cancellation,
) -> Result<Value, BoundaryError> {
    check_cancel(cancel)?;
    publish_prepared_with_roots(prepare_target(target)?, bytes, workspaces, mode, cancel)
}
fn publish_prepared_with_roots(
    target: PreparedTarget,
    bytes: &[u8],
    workspaces: &[&Path],
    mode: WorkMode,
    cancel: &dyn Cancellation,
) -> Result<Value, BoundaryError> {
    check_cancel(cancel)?;
    let published_name = target
        .path
        .to_str()
        .ok_or_else(|| failure("invalid_export_path", "export path must be UTF-8"))?;
    if mode == WorkMode::Plan {
        for workspace in workspaces {
            let root = std::fs::canonicalize(workspace)
                .map_err(|_| failure("invalid_workspace", "workspace is unavailable"))?;
            if target.parent.starts_with(root) {
                return Err(failure(
                    "plan_write_denied",
                    "PLAN export target must be outside the workspace",
                ));
            }
        }
    }
    let check_parent = || target.check_parent();
    check_parent()?;
    let dir = &target.dir;
    let name = &target.name;
    match dir.symlink_metadata(name) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => {
            return Err(failure(
                "export_exists",
                "export target already exists or is inaccessible",
            ));
        }
    }
    let temporary = format!(".saya-export-{}.tmp", RunId::new().0);
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    dir.create_dir_with(&temporary, &builder)
        .map_err(|_| failure("export_io", "cannot create private staging directory"))?;
    let mut staging_options = OpenOptions::new();
    staging_options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY);
    let staging = match dir.open_with(&temporary, &staging_options) {
        Ok(file) => Dir::from_std_file(file.into_std()),
        Err(_) => {
            let _ = dir.remove_dir(&temporary);
            return Err(failure(
                "export_io",
                "cannot open private staging directory",
            ));
        }
    };
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true).mode(0o600);
    let mut file = match staging.open_with("content", &options) {
        Ok(file) => file,
        Err(_) => {
            let _ = dir.remove_dir(&temporary);
            return Err(failure("export_io", "cannot create private export file"));
        }
    };
    let mut published = false;
    let result = (|| {
        for chunk in bytes.chunks(65_536) {
            check_cancel(cancel)?;
            file.write_all(chunk)
                .map_err(|_| failure("export_io", "cannot write export file"))?;
        }
        file.sync_all()
            .map_err(|_| failure("export_io", "cannot synchronize export file"))?;
        check_cancel(cancel)?;
        check_parent()?;
        staging.hard_link("content", dir, name).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                failure("export_exists", "export target already exists")
            } else {
                failure("export_io", "cannot publish export file")
            }
        })?;
        published = true;
        staging
            .remove_file("content")
            .and_then(|()| dir.remove_dir(&temporary))
            .map_err(|_| {
                failure(
                    "export_publish_uncertain",
                    "export published but temporary cleanup failed",
                )
            })?;
        dir.open(".")
            .and_then(|file| file.sync_all())
            .map_err(|_| {
                failure(
                    "export_publish_uncertain",
                    "export published but directory synchronization failed",
                )
            })?;
        let mut read_options = OpenOptions::new();
        read_options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let mut final_file = dir
            .open_with(name, &read_options)
            .map_err(|_| failure("export_publish_uncertain", "cannot verify published export"))?;
        let original = file
            .metadata()
            .map_err(|_| failure("export_publish_uncertain", "cannot inspect export file"))?;
        let actual = final_file.metadata().map_err(|_| {
            failure(
                "export_publish_uncertain",
                "cannot inspect published export",
            )
        })?;
        if !actual.is_file()
            || actual.len() != bytes.len() as u64
            || actual.ino() != original.ino()
            || actual.dev() != original.dev()
        {
            return Err(failure(
                "export_publish_uncertain",
                "published export identity changed",
            ));
        }
        let expected = Sha256::digest(bytes);
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 65_536];
        let mut remaining = bytes.len() as u64 + 1;
        while remaining > 0 {
            let count = Read::by_ref(&mut final_file)
                .take(remaining.min(buffer.len() as u64))
                .read(&mut buffer)
                .map_err(|_| {
                    failure("export_publish_uncertain", "cannot verify published export")
                })?;
            if count == 0 {
                break;
            }
            remaining -= count as u64;
            digest.update(&buffer[..count]);
        }
        if digest.finalize() != expected {
            return Err(failure(
                "export_publish_uncertain",
                "published export content changed",
            ));
        }
        check_parent().map_err(|_| {
            failure(
                "export_publish_uncertain",
                "export published but directory identity changed",
            )
        })?;
        Ok(
            json!({"path":published_name,"bytes":bytes.len(),"sha256":expected.iter().map(|byte| format!("{byte:02x}")).collect::<String>()}),
        )
    })();
    if result.is_err() && !published {
        let _ = staging.remove_file("content");
        let _ = dir.remove_dir(&temporary);
    }
    result
}
