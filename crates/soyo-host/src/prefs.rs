//! User-level desktop preferences and global instructions files under `~/.soyo/`.
//! These are plain file ops and never require a loaded runtime config.

use soyo_ipc::types::{DesktopError, DesktopPrefsDto, GlobalPromptDto};
use std::path::{Path, PathBuf};

fn home_dir() -> Result<PathBuf, DesktopError> {
    let home = std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| DesktopError::new("invalid_config", "HOME is not set"))?;
    Ok(PathBuf::from(home))
}

fn soyo_home_dir() -> Result<PathBuf, DesktopError> {
    Ok(home_dir()?.join(".soyo"))
}

fn desktop_prefs_path() -> Result<PathBuf, DesktopError> {
    Ok(soyo_home_dir()?.join("desktop_prefs.json"))
}

pub fn load_desktop_prefs() -> DesktopPrefsDto {
    let Ok(path) = desktop_prefs_path() else {
        return DesktopPrefsDto {
            nickname: None,
            avatar_data: None,
        };
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<DesktopPrefsDto>(&raw).ok())
        .unwrap_or(DesktopPrefsDto {
            nickname: None,
            avatar_data: None,
        })
}

fn save_desktop_prefs(prefs: &DesktopPrefsDto) -> Result<(), DesktopError> {
    let path = desktop_prefs_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            DesktopError::new(
                "io_error",
                format!("Failed to create {}: {err}", parent.display()),
            )
        })?;
    }
    let raw = serde_json::to_string_pretty(prefs)
        .map_err(|err| DesktopError::new("io_error", format!("Failed to encode prefs: {err}")))?;
    std::fs::write(&path, raw).map_err(|err| {
        DesktopError::new(
            "io_error",
            format!("Failed to write {}: {err}", path.display()),
        )
    })
}

const AVATAR_MAX_BYTES: u64 = 4 * 1024 * 1024;

fn read_avatar_data_url(source: &str) -> Result<String, DesktopError> {
    let path = Path::new(source);
    let meta = std::fs::metadata(path).map_err(|err| {
        DesktopError::new("io_error", format!("Cannot read {}: {err}", path.display()))
    })?;
    if meta.len() > AVATAR_MAX_BYTES {
        return Err(DesktopError::new(
            "invalid_input",
            "Avatar image exceeds the 4 MB limit",
        ));
    }
    let mime = match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        _ => {
            return Err(DesktopError::new(
                "invalid_input",
                "Unsupported avatar format; use PNG, JPEG, WebP or GIF",
            ));
        }
    };
    let bytes = std::fs::read(path).map_err(|err| {
        DesktopError::new("io_error", format!("Cannot read {}: {err}", path.display()))
    })?;
    use base64::{Engine, engine::general_purpose::STANDARD};
    Ok(format!("data:{mime};base64,{}", STANDARD.encode(bytes)))
}

pub async fn get_desktop_prefs() -> Result<DesktopPrefsDto, DesktopError> {
    Ok(load_desktop_prefs())
}

pub async fn set_nickname(nickname: Option<String>) -> Result<DesktopPrefsDto, DesktopError> {
    let mut prefs = load_desktop_prefs();
    prefs.nickname = nickname
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    save_desktop_prefs(&prefs)?;
    Ok(prefs)
}

pub async fn set_avatar(source_path: Option<String>) -> Result<DesktopPrefsDto, DesktopError> {
    let mut prefs = load_desktop_prefs();
    prefs.avatar_data = match source_path {
        Some(path) => Some(read_avatar_data_url(&path)?),
        None => None,
    };
    save_desktop_prefs(&prefs)?;
    Ok(prefs)
}

pub async fn get_user_name() -> Result<Option<String>, DesktopError> {
    Ok(std::env::var("USER")
        .ok()
        .or_else(|| std::env::var("USERNAME").ok())
        .filter(|name| !name.trim().is_empty()))
}

fn global_prompt_target() -> Result<PathBuf, DesktopError> {
    Ok(soyo_home_dir()?.join("AGENTS.md"))
}

fn global_prompt_candidates() -> Result<Vec<PathBuf>, DesktopError> {
    let home = home_dir()?;
    Ok([".soyo/AGENTS.md", ".saya/AGENTS.md", ".agents/AGENTS.md"]
        .iter()
        .map(|rel| home.join(rel))
        .collect())
}

pub async fn get_global_prompt() -> Result<GlobalPromptDto, DesktopError> {
    let target = global_prompt_target()?;
    for candidate in global_prompt_candidates()? {
        if candidate.is_file() {
            let content = std::fs::read_to_string(&candidate).map_err(|err| {
                DesktopError::new(
                    "io_error",
                    format!("Failed to read {}: {err}", candidate.display()),
                )
            })?;
            return Ok(GlobalPromptDto {
                content,
                path: target.to_string_lossy().into_owned(),
                source_path: (candidate != target)
                    .then(|| candidate.to_string_lossy().into_owned()),
            });
        }
    }
    Ok(GlobalPromptDto {
        content: String::new(),
        path: target.to_string_lossy().into_owned(),
        source_path: None,
    })
}

pub async fn set_global_prompt(content: String) -> Result<GlobalPromptDto, DesktopError> {
    let target = global_prompt_target()?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            DesktopError::new(
                "io_error",
                format!("Failed to create {}: {err}", parent.display()),
            )
        })?;
    }
    std::fs::write(&target, &content).map_err(|err| {
        DesktopError::new(
            "io_error",
            format!("Failed to write {}: {err}", target.display()),
        )
    })?;
    Ok(GlobalPromptDto {
        content,
        path: target.to_string_lossy().into_owned(),
        source_path: None,
    })
}
