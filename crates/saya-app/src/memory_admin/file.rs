use super::*;
use std::{
    fs::OpenOptions,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
};

pub(super) fn selected(
    path: &Path,
    config: &Config,
    secrets: &SecretRegistry,
) -> Result<PathBuf, BoundaryError> {
    if path.as_os_str().is_empty() {
        return Err(failure(
            "invalid_args",
            "Select an explicit memory administration file",
        ));
    }
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        config.cwd.join(path)
    };
    let text = path
        .to_str()
        .filter(|text| {
            text.len() <= 4096 && !text.chars().any(saya_core::text::is_terminal_control)
        })
        .ok_or_else(|| failure("invalid_args", "Memory administration path is invalid"))?;
    if saya_core::redaction::contains_secret(text, &secrets.snapshot()?) {
        return Err(failure(
            "sensitive_data",
            "Memory path contains protected data",
        ));
    }
    Ok(path)
}
pub(super) fn read(
    path: &Path,
    config: &Config,
    secrets: &SecretRegistry,
    cancel: &AtomicBool,
) -> Result<String, BoundaryError> {
    let path = selected(path, config, secrets)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|_| failure("memory_input_io", "Memory input is unavailable or linked"))?;
    let before = file
        .metadata()
        .map_err(|_| failure("memory_input_io", "Memory input metadata is unavailable"))?;
    if !before.is_file() || before.len() > MAX_INPUT_BYTES as u64 {
        return Err(failure(
            "memory_input_limit",
            "Memory input must be a bounded regular file",
        ));
    }
    check_cancel(cancel)?;
    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file)
        .take(MAX_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("memory_input_io", "Memory input cannot be read"))?;
    let after = file
        .metadata()
        .map_err(|_| failure("memory_input_io", "Memory input metadata is unavailable"))?;
    let named = std::fs::symlink_metadata(&path)
        .map_err(|_| failure("memory_input_changed", "Memory input changed while reading"))?;
    if !named.is_file()
        || before.dev() != named.dev()
        || before.ino() != named.ino()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
        || bytes.len() as u64 != after.len()
        || bytes.len() > MAX_INPUT_BYTES
    {
        return Err(failure(
            "memory_input_changed",
            "Memory input changed while reading",
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| failure("memory_format", "Memory input must be UTF-8"))?;
    if saya_core::redaction::contains_secret(&text, &secrets.snapshot()?) {
        return Err(failure(
            "sensitive_data",
            "Memory input contains protected data",
        ));
    }
    check_cancel(cancel)?;
    Ok(text)
}
