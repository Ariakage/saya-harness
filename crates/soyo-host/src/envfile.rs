//! Helper-private environment overlay file (`<helper_dir>/env`, mode 0600).
//!
//! GUI-launched clients cannot rely on shell-exported variables, so the
//! first-run setup stores API key *values* here instead of in `config.toml`
//! (which per baseline only references variable names). The helper folds the
//! file into the captured `Environment` before `config::load` validates
//! `api_key_env` references, so downstream auth, redaction and validation see
//! ordinary environment variables.

use soyo_core::{config, contract::BoundaryError};
use soyo_ipc::socket::helper_dir;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};

fn error(code: &str, message: impl Into<String>) -> BoundaryError {
    BoundaryError::new(code, message.into())
}

/// `<helper_dir>/env` — respects `SOYO_HELPER_DIR` so tests and multi-instance
/// setups stay isolated.
pub fn env_file_path() -> PathBuf {
    helper_dir().join("env")
}

/// Parse the overlay file into `(name, value)` pairs. Missing or unreadable
/// files yield an empty overlay; blank lines, `#` comments and malformed lines
/// are skipped so a partially corrupt file cannot block startup.
pub fn load_env_pairs() -> Vec<(OsString, OsString)> {
    let Ok(text) = fs::read_to_string(env_file_path()) else {
        return Vec::new();
    };
    parse_env_pairs(&text)
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

/// Insert or replace `name=value` in the overlay file, preserving other
/// entries. The file is written with mode 0600 inside the helper directory.
/// `name` must pass `valid_env_name`; `value` may not contain newlines or NUL.
pub fn upsert_env_pair(name: &str, value: &str) -> Result<PathBuf, BoundaryError> {
    if !config::valid_env_name(name) {
        return Err(error("invalid_env_name", "Invalid environment name"));
    }
    if value.is_empty() || value.contains(['\n', '\r', '\0']) {
        return Err(error("invalid_env_value", "Invalid environment value"));
    }
    let path = env_file_path();
    let dir = path
        .parent()
        .ok_or_else(|| error("env_path", "Invalid helper env path"))?;
    soyo_fs::ensure_root_dir(dir, "env_io", "helper environment")?;

    let mut entries: BTreeMap<String, String> =
        parse_env_pairs(&fs::read_to_string(&path).unwrap_or_default())
            .into_iter()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect();
    entries.insert(name.to_owned(), value.to_owned());

    let mut text = String::from(
        "# Soyo helper environment overlay — values are referenced by name from config.toml.\n",
    );
    for (key, val) in &entries {
        text.push_str(key);
        text.push('=');
        text.push_str(val);
        text.push('\n');
    }

    let tmp = path.with_file_name("env.tmp");
    {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
            .open(&tmp)
            .map_err(|_| error("env_io", "Cannot write helper environment file"))?;
        file.write_all(text.as_bytes())
            .map_err(|_| error("env_io", "Cannot write helper environment file"))?;
    }
    #[cfg(unix)]
    fs::rename(&tmp, &path)
        .map_err(|_| error("env_io", "Cannot replace helper environment file"))?;
    #[cfg(not(unix))]
    {
        let _ = fs::remove_file(&path);
        fs::rename(&tmp, &path)
            .map_err(|_| error("env_io", "Cannot replace helper environment file"))?;
    }
    Ok(path)
}
