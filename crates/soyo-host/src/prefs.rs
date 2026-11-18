//! User-level desktop preferences and global instructions files under `~/.soyo/`.
//! Blocking file IO is offloaded to the blocking pool (`tokio::task::spawn_blocking`).

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
        return DesktopPrefsDto::default();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<DesktopPrefsDto>(&raw).ok())
        .unwrap_or_default()
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
    let tmp = path.with_file_name(format!("desktop_prefs.tmp.{}", std::process::id()));
    std::fs::write(&tmp, raw).map_err(|err| {
        DesktopError::new(
            "io_error",
            format!("Failed to write {}: {err}", tmp.display()),
        )
    })?;
    std::fs::rename(&tmp, &path).map_err(|err| {
        DesktopError::new(
            "io_error",
            format!("Failed to replace {}: {err}", path.display()),
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

async fn run_prefs_blocking<F, T>(f: F) -> Result<T, DesktopError>
where
    F: FnOnce() -> Result<T, DesktopError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| DesktopError::new("io_error", "Desktop preferences worker failed"))?
}

pub async fn get_desktop_prefs() -> Result<DesktopPrefsDto, DesktopError> {
    run_prefs_blocking(|| Ok(load_desktop_prefs())).await
}

pub async fn set_nickname(nickname: Option<String>) -> Result<DesktopPrefsDto, DesktopError> {
    run_prefs_blocking(move || {
        let mut prefs = load_desktop_prefs();
        prefs.nickname = nickname
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        save_desktop_prefs(&prefs)?;
        Ok(prefs)
    })
    .await
}

pub async fn set_avatar(source_path: Option<String>) -> Result<DesktopPrefsDto, DesktopError> {
    run_prefs_blocking(move || {
        let mut prefs = load_desktop_prefs();
        prefs.avatar_data = match source_path {
            Some(path) => Some(read_avatar_data_url(&path)?),
            None => None,
        };
        save_desktop_prefs(&prefs)?;
        Ok(prefs)
    })
    .await
}

pub async fn update_desktop_prefs(
    theme: Option<String>,
    font_size: Option<u16>,
    conversation_font: Option<String>,
    reduce_motion: Option<bool>,
    inspector_open: Option<bool>,
) -> Result<DesktopPrefsDto, DesktopError> {
    run_prefs_blocking(move || {
        let mut prefs = load_desktop_prefs();
        if let Some(theme) = theme {
            match theme.as_str() {
                "system" | "light" | "dark" => prefs.theme = Some(theme),
                _ => {
                    return Err(DesktopError::new(
                        "invalid_args",
                        "Theme must be 'system', 'light', or 'dark'",
                    ));
                }
            }
        }
        if let Some(size) = font_size {
            if matches!(size, 13 | 14 | 16) {
                prefs.font_size = Some(size);
            } else {
                return Err(DesktopError::new(
                    "invalid_args",
                    "Font size must be 13, 14, or 16",
                ));
            }
        }
        if let Some(font) = conversation_font {
            match font.as_str() {
                "default" | "serif" | "system" => prefs.conversation_font = Some(font),
                _ => {
                    return Err(DesktopError::new(
                        "invalid_args",
                        "Conversation font must be 'default', 'serif', or 'system'",
                    ));
                }
            }
        }
        if let Some(reduce) = reduce_motion {
            prefs.reduce_motion = Some(reduce);
        }
        if let Some(open) = inspector_open {
            prefs.inspector_open = Some(open);
        }
        save_desktop_prefs(&prefs)?;
        Ok(prefs)
    })
    .await
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
    run_prefs_blocking(|| {
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
    })
    .await
}

pub async fn set_global_prompt(content: String) -> Result<GlobalPromptDto, DesktopError> {
    run_prefs_blocking(move || {
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
    })
    .await
}
