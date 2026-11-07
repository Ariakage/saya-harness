//! Explicit file publication for rendered session exports.
use cap_std::fs::{Dir, DirBuilder, DirBuilderExt, MetadataExt as _, OpenOptions, OpenOptionsExt};
use saya_core::{
    boundary::Cancellation,
    contract::{BoundaryError, ErrorInfo, RunId, WorkMode},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::Path,
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
    check_cancel(cancel)?;
    let name = target
        .file_name()
        .ok_or_else(|| failure("invalid_export_path", "export requires a file name"))?;
    let parent = target
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = std::fs::canonicalize(parent).map_err(|_| {
        failure(
            "invalid_export_path",
            "export parent directory is unavailable",
        )
    })?;
    let published_path = parent.join(name);
    let published_name = published_path
        .to_str()
        .ok_or_else(|| failure("invalid_export_path", "export path must be UTF-8"))?;
    if mode == WorkMode::Plan {
        let root = std::fs::canonicalize(workspace)
            .map_err(|_| failure("invalid_workspace", "workspace is unavailable"))?;
        if parent.starts_with(root) {
            return Err(failure(
                "plan_write_denied",
                "PLAN export target must be outside the workspace",
            ));
        }
    }
    use std::os::unix::fs::OpenOptionsExt as _;
    let parent_file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(&parent)
        .map_err(|_| failure("export_io", "cannot open export directory"))?;
    let identity = parent_file
        .metadata()
        .map_err(|_| failure("export_io", "cannot inspect export directory"))?;
    let check_parent = || -> Result<(), BoundaryError> {
        let canonical = std::fs::canonicalize(&parent)
            .map_err(|_| failure("export_path_changed", "export directory changed"))?;
        let actual = std::fs::symlink_metadata(&parent)
            .map_err(|_| failure("export_path_changed", "export directory changed"))?;
        if canonical != parent
            || !actual.is_dir()
            || actual.ino() != identity.ino()
            || actual.dev() != identity.dev()
        {
            return Err(failure("export_path_changed", "export directory changed"));
        }
        Ok(())
    };
    check_parent()?;
    let dir = Dir::from_std_file(parent_file);
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
        staging.hard_link("content", &dir, name).map_err(|error| {
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
