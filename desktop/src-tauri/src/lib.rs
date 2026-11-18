//! Tauri 2 in-process desktop shell for Soyo Harness.

use serde::{Deserialize, Serialize};
use soyo_app::{offload_boundary, session};
use soyo_core::{
    config::{self, Config, Environment, Overrides},
    contract::{BoundaryError, WorkMode},
    credentials::SecretRegistry,
    redaction,
    sqlite::SessionSummary,
};
use specta::Type;
use specta_typescript::{Number, Typescript};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tauri::State;
use tauri_specta::{Builder, collect_commands};

const SESSIONS_PAGE_SIZE: u64 = 20;

/// Structured IPC error returned to the frontend; sensitive values are redacted.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct DesktopError {
    pub code: String,
    pub message: String,
}

impl DesktopError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn from_boundary(error: &BoundaryError, secrets: &[String]) -> Self {
        let mut message = serde_json::Value::String(error.message().to_owned());
        let safe_message = if redaction::redact(&mut message, secrets).is_ok() {
            message
                .as_str()
                .unwrap_or("sensitive details withheld")
                .to_owned()
        } else {
            "sensitive details withheld".to_owned()
        };
        Self {
            code: error.code().to_owned(),
            message: safe_message,
        }
    }
}

/// Paginated session metadata returned by `list_sessions`.
#[derive(Debug, Clone, Serialize, Type)]
pub struct SessionItemDto {
    pub id: String,
    pub workspace: String,
    pub work_mode: WorkModeDto,
    pub profile: String,
    pub title: Option<String>,
    #[specta(type = Number)]
    pub updated_at: i64,
    pub last_status: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkModeDto {
    Plan,
    Build,
    Code,
    Work,
}

impl From<WorkMode> for WorkModeDto {
    fn from(value: WorkMode) -> Self {
        match value {
            WorkMode::Plan => Self::Plan,
            WorkMode::Build => Self::Build,
            WorkMode::Code => Self::Code,
            WorkMode::Work => Self::Work,
        }
    }
}

#[derive(Debug, Clone, Serialize, Type)]
pub struct SessionPageDto {
    pub page: u32,
    pub page_size: u32,
    pub items: Vec<SessionItemDto>,
}

/// Shared in-process desktop application state.
pub struct DesktopState {
    inner: Arc<Mutex<DesktopInner>>,
    secrets: Arc<SecretRegistry>,
}

struct DesktopInner {
    config: Option<Config>,
    config_error: Option<String>,
}

impl DesktopState {
    pub fn from_environment() -> Self {
        let env = Environment::capture();
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        match config::load(Overrides::default(), env, &cwd) {
            Ok(cfg) => Self::with_config(cfg),
            Err(err) => Self {
                inner: Arc::new(Mutex::new(DesktopInner {
                    config: None,
                    config_error: Some(err.message().to_owned()),
                })),
                secrets: Arc::new(SecretRegistry::default()),
            },
        }
    }

    pub fn with_config(cfg: Config) -> Self {
        let registry = Arc::new(SecretRegistry::default());
        let known = cfg
            .secret_env_names()
            .iter()
            .filter_map(|name| cfg.secret(name).ok().map(str::to_owned))
            .collect::<Vec<_>>();
        let _ = registry.register(&known);
        Self {
            inner: Arc::new(Mutex::new(DesktopInner {
                config: Some(cfg),
                config_error: None,
            })),
            secrets: registry,
        }
    }

    fn current_secrets(&self) -> Vec<String> {
        self.secrets.snapshot().unwrap_or_default()
    }

    fn config_snapshot(&self) -> Result<Config, DesktopError> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| DesktopError::new("state_lock", "desktop state lock failed"))?;
        if let Some(cfg) = &guard.config {
            Ok(cfg.clone())
        } else {
            Err(DesktopError::new(
                "invalid_config",
                guard
                    .config_error
                    .as_deref()
                    .unwrap_or("configuration is not loaded"),
            ))
        }
    }

    pub async fn list_sessions_page(
        &self,
        page: Option<u32>,
    ) -> Result<SessionPageDto, DesktopError> {
        let page_num = page.unwrap_or(1).max(1);
        let offset = u64::from(page_num - 1) * SESSIONS_PAGE_SIZE;
        let cfg = self.config_snapshot()?;
        let secrets = self.current_secrets();
        let store = offload_boundary(move || {
            session::open(&cfg).map_err(|err| BoundaryError::new("storage_error", err.to_string()))
        })
        .await
        .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
        let summaries = store
            .list_sessions(offset, SESSIONS_PAGE_SIZE)
            .await
            .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;

        let items = summaries
            .into_iter()
            .map(|summary| redact_summary(summary, &secrets))
            .collect();
        Ok(SessionPageDto {
            page: page_num,
            page_size: SESSIONS_PAGE_SIZE as u32,
            items,
        })
    }
}

fn redact_string(raw: &str, secrets: &[String]) -> String {
    let mut value = serde_json::Value::String(raw.to_owned());
    if redaction::redact(&mut value, secrets).is_ok() {
        value
            .as_str()
            .unwrap_or("sensitive details withheld")
            .to_owned()
    } else {
        "sensitive details withheld".to_owned()
    }
}

fn redact_summary(summary: SessionSummary, secrets: &[String]) -> SessionItemDto {
    SessionItemDto {
        id: summary.id.0.to_string(),
        workspace: redact_string(&summary.workspace.to_string_lossy(), secrets),
        work_mode: summary.work_mode.into(),
        profile: redact_string(&summary.profile, secrets),
        title: summary.title.map(|t| redact_string(&t, secrets)),
        updated_at: summary.updated_at,
        last_status: summary
            .last_status
            .map(|status| format!("{status:?}").to_lowercase()),
    }
}

#[tauri::command]
#[specta::specta]
async fn list_sessions(
    state: State<'_, DesktopState>,
    page: Option<u32>,
) -> Result<SessionPageDto, DesktopError> {
    state.list_sessions_page(page).await
}

pub fn specta_builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new().commands(collect_commands![list_sessions])
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = specta_builder();
    let desktop_state = DesktopState::from_environment();
    tauri::Builder::default()
        .manage(desktop_state)
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(builder.invoke_handler())
        .setup(move |app| {
            builder.mount_events(app);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Soyo desktop application");
}

pub fn export_bindings_to(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    specta_builder().export(Typescript::default(), path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_specta_bindings() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src/bindings.ts");
        export_bindings_to(&path).expect("specta bindings export should succeed");
        let content = std::fs::read_to_string(&path).expect("bindings.ts should exist");
        assert!(content.contains("listSessions"));
    }

    #[tokio::test]
    async fn list_sessions_returns_redacted_empty_page_for_fresh_store() {
        let root = tempfile::tempdir().unwrap();
        let cfg_path = root.path().join("config.toml");
        std::fs::write(&cfg_path, "schema_version = 1\n").unwrap();
        let cfg = config::load(
            Overrides {
                config: Some(cfg_path),
                ..Default::default()
            },
            Environment::from_pairs([("HOME".into(), root.path().as_os_str().to_owned())]),
            root.path(),
        )
        .unwrap();
        let state = DesktopState::with_config(cfg);
        let page = state.list_sessions_page(Some(1)).await.unwrap();
        assert_eq!(page.page, 1);
        assert!(page.items.is_empty());
    }
}
