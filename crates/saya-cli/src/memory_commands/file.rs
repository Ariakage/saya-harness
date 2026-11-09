use super::*;
use saya_core::boundary::Cancellation;
use std::{
    fs::OpenOptions,
    future::Future,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    pin::Pin,
};

fn selected(
    path: &Path,
    config: &Config,
    secrets: &SecretRegistry,
) -> Result<PathBuf, BoundaryError> {
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
            "Memory administration path contains protected data",
        ));
    }
    Ok(path)
}
pub(super) fn read(
    path: &Path,
    config: &Config,
    max: usize,
    secrets: &SecretRegistry,
    cancel: &AtomicBool,
) -> Result<String, BoundaryError> {
    check_cancel(cancel)?;
    let path = selected(path, config, secrets)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|_| {
            failure(
                "memory_input_io",
                "Memory administration input is unavailable or linked",
            )
        })?;
    let before = file.metadata().map_err(|_| {
        failure(
            "memory_input_io",
            "Memory administration input metadata is unavailable",
        )
    })?;
    if !before.is_file() || before.len() > max as u64 {
        return Err(failure(
            "memory_input_limit",
            "Memory administration input must be a bounded regular file",
        ));
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    let mut chunk = [0; 16 * 1024];
    loop {
        check_cancel(cancel)?;
        let count = file.read(&mut chunk).map_err(|_| {
            failure(
                "memory_input_io",
                "Memory administration input cannot be read",
            )
        })?;
        if count == 0 {
            break;
        }
        if bytes.len().saturating_add(count) > max {
            return Err(failure(
                "memory_input_limit",
                "Memory administration input exceeds its byte limit",
            ));
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    if bytes.len() > max {
        return Err(failure(
            "memory_input_limit",
            "Memory administration input exceeds its byte limit",
        ));
    }
    let after = file.metadata().map_err(|_| {
        failure(
            "memory_input_io",
            "Memory administration input metadata is unavailable",
        )
    })?;
    let named = std::fs::symlink_metadata(&path).map_err(|_| {
        failure(
            "memory_input_changed",
            "Memory administration input changed while reading",
        )
    })?;
    if !named.is_file()
        || before.dev() != named.dev()
        || before.ino() != named.ino()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
        || bytes.len() as u64 != after.len()
    {
        return Err(failure(
            "memory_input_changed",
            "Memory administration input changed while reading",
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| failure("memory_format", "Memory administration input must be UTF-8"))?;
    if saya_core::redaction::contains_secret(&text, &secrets.snapshot()?) {
        return Err(failure(
            "sensitive_data",
            "Memory administration input contains protected data",
        ));
    }
    check_cancel(cancel)?;
    Ok(text)
}
struct ExplicitAdministration<'a>(&'a AtomicBool);
impl Cancellation for ExplicitAdministration<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}
pub(super) fn publish(
    path: &Path,
    bytes: &[u8],
    config: &Config,
    secrets: &SecretRegistry,
    cancel: &AtomicBool,
) -> Result<Value, BoundaryError> {
    check_cancel(cancel)?;
    let target = selected(path, config, secrets)?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(failure(
            "memory_output_limit",
            "Memory backup exceeds its publication byte limit",
        ));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| failure("memory_format", "Memory backup encoding is invalid"))?;
    if saya_core::redaction::contains_secret(text, &secrets.snapshot()?) {
        return Err(failure(
            "sensitive_data",
            "Memory backup publication contains protected data",
        ));
    }
    saya_app::export::publish(
        &target,
        bytes,
        &config.cwd,
        config.mode,
        &ExplicitAdministration(cancel),
    )
}
