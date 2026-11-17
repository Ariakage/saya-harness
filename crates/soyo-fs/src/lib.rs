//! Shared Unix private-filesystem, symlink-safe opening, flock locking, and bounded reading primitives.

use soyo_contract::BoundaryError;
use std::{
    fs::{self, DirBuilder, File, Metadata, OpenOptions},
    io::{ErrorKind, Read},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

/// Ensure `root` exists as a non-symlink directory (`0700` when newly created).
pub fn ensure_root_dir(root: &Path, code: &str, subject: &str) -> Result<(), BoundaryError> {
    match fs::symlink_metadata(root) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_dir() {
                return Err(BoundaryError::new(
                    code,
                    format!("{subject} root must be a real directory"),
                ));
            }
            Ok(())
        }
        Err(err) if err.kind() == ErrorKind::NotFound => DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)
            .map_err(|_| {
                BoundaryError::new(code, format!("cannot create {subject} root directory"))
            }),
        Err(_) => Err(BoundaryError::new(
            code,
            format!("cannot inspect {subject} root directory"),
        )),
    }
}

/// Ensure `dir` exists with `0700` permissions, open it without following symlinks,
/// verify ownership and privacy (`mode & 0o077 == 0`), and return `(file, metadata, canonical_path)`.
pub fn ensure_private_dir(
    dir: &Path,
    code: &str,
    subject: &str,
) -> Result<(File, Metadata, PathBuf), BoundaryError> {
    match DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
        Err(_) => {
            return Err(BoundaryError::new(
                code,
                format!("cannot create private {subject} directory"),
            ));
        }
    }
    let (file, meta) = open_existing_private_dir(dir, code, subject)?.ok_or_else(|| {
        BoundaryError::new(
            code,
            format!("{subject} directory disappeared while opening"),
        )
    })?;
    let canonical = verify_canonical_dir(dir, &meta, code, subject)?;
    Ok((file, meta, canonical))
}

/// Open an existing `dir` without following symlinks and verify ownership and `0700` privacy.
/// Returns `Ok(None)` when `dir` does not exist.
pub fn open_existing_private_dir(
    dir: &Path,
    code: &str,
    subject: &str,
) -> Result<Option<(File, Metadata)>, BoundaryError> {
    let named = match fs::symlink_metadata(dir) {
        Ok(meta) => meta,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(BoundaryError::new(
                code,
                format!("cannot inspect {subject} directory"),
            ));
        }
    };
    if named.file_type().is_symlink() || !named.is_dir() {
        return Err(BoundaryError::new(
            code,
            format!("{subject} directory must not be a symlink"),
        ));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(dir)
        .map_err(|_| BoundaryError::new(code, format!("cannot open {subject} directory safely")))?;
    let meta = file.metadata().map_err(|_| {
        BoundaryError::new(code, format!("cannot read {subject} directory metadata"))
    })?;
    verify_dir_metadata(&meta, code, subject)?;
    if (meta.dev(), meta.ino()) != (named.dev(), named.ino()) {
        return Err(BoundaryError::new(
            code,
            format!("{subject} directory changed while opening"),
        ));
    }
    Ok(Some((file, meta)))
}

/// Canonicalize `dir` and verify it resolves to the same device/inode as `dir_meta`.
pub fn verify_canonical_dir(
    dir: &Path,
    dir_meta: &Metadata,
    code: &str,
    subject: &str,
) -> Result<PathBuf, BoundaryError> {
    let canonical = fs::canonicalize(dir)
        .map_err(|_| BoundaryError::new(code, format!("cannot resolve {subject} directory")))?;
    let named = fs::symlink_metadata(&canonical).map_err(|_| {
        BoundaryError::new(code, format!("{subject} directory identity is unavailable"))
    })?;
    if !named.is_dir() || (named.dev(), named.ino()) != (dir_meta.dev(), dir_meta.ino()) {
        return Err(BoundaryError::new(
            code,
            format!("{subject} directory changed while opening"),
        ));
    }
    Ok(canonical)
}

/// Check that directory metadata represents a real directory owned by the current effective UID
/// with no group or world permission bits (`mode & 0o077 == 0`).
pub fn verify_dir_metadata(
    meta: &Metadata,
    code: &str,
    subject: &str,
) -> Result<(), BoundaryError> {
    // SAFETY: `geteuid` takes no pointers and retains no ownership.
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(BoundaryError::new(
            code,
            format!("{subject} directory must be owned and private"),
        ));
    }
    Ok(())
}

/// Check that `path` and its open descriptor `file` match a single-link private regular file
/// owned by the current effective UID with no group or world permission bits (`mode & 0o077 == 0`).
pub fn verify_private_regular_file(
    path: &Path,
    file: &File,
    code: &str,
    subject: &str,
) -> Result<Metadata, BoundaryError> {
    let named = fs::symlink_metadata(path)
        .map_err(|_| BoundaryError::new(code, format!("{subject} file identity unavailable")))?;
    let meta = file
        .metadata()
        .map_err(|_| BoundaryError::new(code, format!("{subject} file metadata unavailable")))?;
    // SAFETY: `geteuid` takes no pointers and retains no ownership.
    if named.file_type().is_symlink()
        || !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || (meta.dev(), meta.ino()) != (named.dev(), named.ino())
    {
        return Err(BoundaryError::new(
            code,
            format!("{subject} files must be owned private regular files"),
        ));
    }
    Ok(meta)
}

/// Open an existing private regular file for reading (`O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK`).
/// Returns `Ok(None)` when `path` does not exist.
pub fn open_existing_private_file_read(
    path: &Path,
    code: &str,
    subject: &str,
) -> Result<Option<(File, Metadata)>, BoundaryError> {
    let named = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(BoundaryError::new(
                code,
                format!("cannot inspect {subject} file"),
            ));
        }
    };
    if named.file_type().is_symlink() || !named.is_file() {
        return Err(BoundaryError::new(
            code,
            format!("{subject} must be a regular file"),
        ));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| BoundaryError::new(code, format!("cannot open {subject} safely")))?;
    let meta = verify_private_regular_file(path, &file, code, subject)?;
    Ok(Some((file, meta)))
}

/// Atomically create a new `0600` regular file (`create_new`, `O_NOFOLLOW | O_CLOEXEC`), or verify
/// an existing private file when `AlreadyExists` occurs. Returns `(created, metadata)`.
pub fn create_or_verify_private_file(
    path: &Path,
    code: &str,
    subject: &str,
) -> Result<(bool, Metadata), BoundaryError> {
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => {
            let meta = verify_private_regular_file(path, &file, code, subject)?;
            Ok((true, meta))
        }
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {
            let (_file, meta) =
                open_existing_private_file_read(path, code, subject)?.ok_or_else(|| {
                    BoundaryError::new(code, format!("{subject} disappeared while opening"))
                })?;
            Ok((false, meta))
        }
        Err(_) => Err(BoundaryError::new(
            code,
            format!("cannot create {subject} file"),
        )),
    }
}

/// Open or create a private `0600` lock file and acquire an exclusive `flock` within `timeout`.
pub fn acquire_flock(
    lock_path: &Path,
    timeout: Duration,
    busy_code: &str,
    error_code: &str,
    subject: &str,
) -> Result<File, BoundaryError> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(lock_path)
        .map_err(|_| BoundaryError::new(error_code, format!("cannot open {subject} lock file")))?;
    verify_private_regular_file(lock_path, &lock, error_code, subject)?;
    let deadline = Instant::now() + timeout;
    loop {
        // SAFETY: `lock` owns a valid regular file descriptor for this scope.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(lock);
        }
        let cause = std::io::Error::last_os_error();
        if cause.kind() != ErrorKind::WouldBlock {
            return Err(BoundaryError::new(
                error_code,
                format!("cannot acquire {subject} lock"),
            ));
        }
        if Instant::now() >= deadline {
            return Err(BoundaryError::new(
                busy_code,
                format!("{subject} lock timed out"),
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Read a bounded regular UTF-8 file (`O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK`) up to `max_bytes`.
pub fn read_bounded_utf8_file(
    path: &Path,
    max_bytes: usize,
    code: &str,
    subject: &str,
) -> Result<String, BoundaryError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| BoundaryError::new(code, format!("{subject} is unavailable")))?;
    let metadata = file
        .metadata()
        .map_err(|_| BoundaryError::new(code, format!("{subject} is unavailable")))?;
    let limit = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    if !metadata.is_file() || metadata.len() > limit {
        return Err(BoundaryError::new(
            code,
            format!("{subject} is not a bounded regular file"),
        ));
    }
    let mut text = String::new();
    file.take(limit.saturating_add(1))
        .read_to_string(&mut text)
        .map_err(|_| BoundaryError::new(code, format!("{subject} is not UTF-8")))?;
    if text.len() > max_bytes {
        return Err(BoundaryError::new(
            code,
            format!("{subject} exceeded its size limit while being read"),
        ));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use tempfile::tempdir;

    #[test]
    fn private_dir_and_file_primitives_enforce_ownership_and_reject_symlinks() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("private");
        ensure_root_dir(&root, "storage_error", "test").unwrap();

        let sub = root.join("sub");
        let (_file, _meta, canonical) = ensure_private_dir(&sub, "storage_error", "test").unwrap();
        assert!(canonical.ends_with("sub"));

        let db = canonical.join("state.db");
        let (created, _) = create_or_verify_private_file(&db, "storage_error", "test").unwrap();
        assert!(created);
        fs::write(&db, "hello").unwrap();

        let (created_again, meta) =
            create_or_verify_private_file(&db, "storage_error", "test").unwrap();
        assert!(!created_again);
        assert_eq!(meta.len(), 5);

        fs::set_permissions(&db, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(open_existing_private_file_read(&db, "storage_error", "test").is_err());

        let sym = root.join("sym-dir");
        symlink(&sub, &sym).unwrap();
        assert!(open_existing_private_dir(&sym, "storage_error", "test").is_err());
    }

    #[test]
    fn bounded_utf8_file_rejects_oversize_and_symlinks() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("plan.json");
        fs::write(&file, "hello").unwrap();
        assert_eq!(
            read_bounded_utf8_file(&file, 10, "invalid_args", "plan").unwrap(),
            "hello"
        );
        assert!(read_bounded_utf8_file(&file, 4, "invalid_args", "plan").is_err());

        let link = dir.path().join("link.json");
        symlink(&file, &link).unwrap();
        assert!(read_bounded_utf8_file(&link, 10, "invalid_args", "plan").is_err());
    }
}
