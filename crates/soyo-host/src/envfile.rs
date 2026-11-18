//! Helper-private secret environment overlay file (`<state_dir>/secrets/env`, mode 0600).
//!
//! GUI-launched clients cannot rely on shell-exported variables, so the
//! first-run setup stores API key *values* here instead of in `config.toml`
//! (which per baseline only references variable names). The helper folds the
//! file into the captured `Environment` before `config::load` validates
//! `api_key_env` references and registers all loaded values in `SecretRegistry`.

use soyo_core::{config, contract::BoundaryError};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

fn error(code: &str, message: impl Into<String>) -> BoundaryError {
    BoundaryError::new(code, message.into())
}

/// Directory containing the helper-private secret environment file (`<state_dir>/secrets`).
pub fn secrets_dir_for(state_dir: &Path) -> PathBuf {
    state_dir.join("secrets")
}

/// `<state_dir>/secrets/env`.
pub fn env_file_path_for(state_dir: &Path) -> PathBuf {
    secrets_dir_for(state_dir).join("env")
}

/// Migrate legacy `~/.soyo/env` into `<state_dir>/secrets/env` (dir 0700, file 0600)
/// on first startup when the destination file does not exist yet. Never follows
/// symlinks or clobbers an existing destination file.
pub fn migrate_legacy_env(state_dir: &Path) {
    let dest = env_file_path_for(state_dir);
    if dest.exists() {
        return;
    }
    let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) else {
        return;
    };
    let legacy = PathBuf::from(home).join(".soyo").join("env");
    let Ok(named) = fs::symlink_metadata(&legacy) else {
        return;
    };
    // SAFETY: `geteuid` takes no pointers and retains no ownership.
    let uid = unsafe { libc::geteuid() };
    if named.file_type().is_symlink() || !named.is_file() || named.uid() != uid {
        return;
    }
    let Ok(file) = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(&legacy)
    else {
        return;
    };
    let Ok(meta) = file.metadata() else {
        return;
    };
    if !meta.is_file()
        || meta.uid() != uid
        || (meta.dev(), meta.ino()) != (named.dev(), named.ino())
    {
        return;
    }
    let _ = meta.as_raw_fd_unused();
    let mut text = String::new();
    if file.take(256 * 1024).read_to_string(&mut text).is_err() {
        return;
    }
    let pairs: BTreeMap<String, String> = parse_env_pairs(&text)
        .into_iter()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect();
    if write_env_map(state_dir, &pairs).is_ok() {
        let _ = fs::remove_file(&legacy);
    }
}

trait MetadataDummy {
    fn as_raw_fd_unused(&self) -> i32 {
        0
    }
}
impl MetadataDummy for fs::Metadata {}

/// Parse `<state_dir>/secrets/env` into `(name, value)` pairs. Only regular,
/// single-link, non-symlink, `0600` user-owned files inside a private `0700`
/// directory are accepted. Missing files yield an empty overlay.
pub fn load_env_pairs_for(state_dir: &Path) -> Vec<(OsString, OsString)> {
    let secrets_dir = secrets_dir_for(state_dir);
    let Ok(Some(_)) =
        soyo_fs::open_existing_private_dir(&secrets_dir, "env_io", "secrets environment")
    else {
        return Vec::new();
    };
    let path = env_file_path_for(state_dir);
    let Ok(Some((file, _))) =
        soyo_fs::open_existing_private_file_read(&path, "env_io", "secrets environment")
    else {
        return Vec::new();
    };
    let _ = file.as_raw_fd();
    let mut text = String::new();
    if file.take(256 * 1024).read_to_string(&mut text).is_err() {
        return Vec::new();
    }
    parse_env_pairs(&text)
}

/// Extract string secret values loaded from `<state_dir>/secrets/env` for registration
/// with [`soyo_core::security::SecretRegistry`].
pub fn load_env_secret_values_for(state_dir: &Path) -> Vec<String> {
    load_env_pairs_for(state_dir)
        .into_iter()
        .filter_map(|(_, value)| value.into_string().ok())
        .filter(|value| !value.is_empty())
        .collect()
}

fn parse_env_pairs(text: &str) -> Vec<(OsString, OsString)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim_end_matches(['\r', '\n']);
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                return None;
            }
            let (name, value) = line.split_once('=')?;
            let name = name.trim();
            if !config::valid_env_name(name) {
                return None;
            }
            Some((
                OsString::from(name),
                OsString::from(value.trim_matches('\r')),
            ))
        })
        .collect()
}

fn write_env_map(
    state_dir: &Path,
    entries: &BTreeMap<String, String>,
) -> Result<PathBuf, BoundaryError> {
    soyo_fs::ensure_root_dir(state_dir, "env_io", "state")?;
    let secrets_dir = secrets_dir_for(state_dir);
    soyo_fs::ensure_private_dir(&secrets_dir, "env_io", "secrets environment")?;
    let path = env_file_path_for(state_dir);

    let mut text = String::from(
        "# Soyo helper secret environment overlay — values are referenced by name from config.toml.\n",
    );
    for (key, val) in entries {
        text.push_str(key);
        text.push('=');
        text.push_str(val);
        text.push('\n');
    }

    let tmp = secrets_dir.join(format!("env.tmp.{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    {
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let mut file = options
            .open(&tmp)
            .map_err(|_| error("env_io", "Cannot write secrets environment file"))?;
        file.write_all(text.as_bytes())
            .map_err(|_| error("env_io", "Cannot write secrets environment file"))?;
    }
    fs::rename(&tmp, &path)
        .map_err(|_| error("env_io", "Cannot replace secrets environment file"))?;
    Ok(path)
}

/// Insert or replace `name=value` in `<state_dir>/secrets/env`, preserving other
/// entries. Directory is enforced `0700` and file `0600`.
pub fn upsert_env_pair_for(
    state_dir: &Path,
    name: &str,
    value: &str,
) -> Result<PathBuf, BoundaryError> {
    if !config::valid_env_name(name) {
        return Err(error("invalid_env_name", "Invalid environment name"));
    }
    if value.is_empty() || value.contains(['\n', '\r', '\0']) {
        return Err(error("invalid_env_value", "Invalid environment value"));
    }

    let mut entries: BTreeMap<String, String> = load_env_pairs_for(state_dir)
        .into_iter()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect();
    entries.insert(name.to_owned(), value.to_owned());

    write_env_map(state_dir, &entries)
}
